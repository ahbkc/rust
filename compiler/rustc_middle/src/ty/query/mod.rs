use crate::dep_graph;
use crate::hir::exports::Export;
use crate::infer::canonical::{self, Canonical};
use crate::lint::LintLevelMap;
use crate::middle::codegen_fn_attrs::CodegenFnAttrs;
use crate::middle::cstore::{CrateDepKind, CrateSource};
use crate::middle::cstore::{ExternCrate, ForeignModule, LinkagePreference, NativeLib};
use crate::middle::exported_symbols::{ExportedSymbol, SymbolExportLevel};
use crate::middle::lib_features::LibFeatures;
use crate::middle::privacy::AccessLevels;
use crate::middle::region;
use crate::middle::resolve_lifetime::{ObjectLifetimeDefault, Region, ResolveLifetimes};
use crate::middle::stability::{self, DeprecationEntry};
use crate::mir;
use crate::mir::interpret::GlobalId;
use crate::mir::interpret::{ConstAlloc, LitToConstError, LitToConstInput};
use crate::mir::interpret::{ConstValue, EvalToAllocationRawResult, EvalToConstValueResult};
use crate::mir::mono::CodegenUnit;
use crate::traits::query::{
    CanonicalPredicateGoal, CanonicalProjectionGoal, CanonicalTyGoal,
    CanonicalTypeOpAscribeUserTypeGoal, CanonicalTypeOpEqGoal, CanonicalTypeOpNormalizeGoal,
    CanonicalTypeOpProvePredicateGoal, CanonicalTypeOpSubtypeGoal, NoSolution,
};
use crate::traits::query::{
    DropckOutlivesResult, DtorckConstraint, MethodAutoderefStepsResult, NormalizationResult,
    OutlivesBound,
};
use crate::traits::specialization_graph;
use crate::traits::{self, ImplSource};
use crate::ty::subst::{GenericArg, SubstsRef};
use crate::ty::util::AlwaysRequiresDrop;
use crate::ty::{self, AdtSizedConstraint, CrateInherentImpls, ParamEnvAnd, Ty, TyCtxt};
use rustc_data_structures::fx::{FxHashMap, FxHashSet, FxIndexMap};
use rustc_data_structures::stable_hasher::StableVec;
use rustc_data_structures::steal::Steal;
use rustc_data_structures::svh::Svh;
use rustc_data_structures::sync::Lrc;
use rustc_errors::{ErrorReported, Handler};
use rustc_hir as hir;
use rustc_hir::def::DefKind;
use rustc_hir::def_id::{CrateNum, DefId, DefIdMap, DefIdSet, LocalDefId};
use rustc_hir::lang_items::{LangItem, LanguageItems};
use rustc_hir::{Crate, ItemLocalId, TraitCandidate};
use rustc_index::{bit_set::FiniteBitSet, vec::IndexVec};
use rustc_serialize::opaque;
use rustc_session::config::{EntryFnType, OptLevel, OutputFilenames, SymbolManglingVersion};
use rustc_session::utils::NativeLibKind;
use rustc_session::CrateDisambiguator;
use rustc_target::spec::PanicStrategy;

use rustc_ast as ast;
use rustc_attr as attr;
use rustc_span::symbol::Symbol;
use rustc_span::{Span, DUMMY_SP};
use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) use rustc_query_system::query::QueryJobId;
use rustc_query_system::query::*;

pub mod on_disk_cache;
pub use self::on_disk_cache::OnDiskCache;

#[derive(Copy, Clone)]
pub struct TyCtxtAt<'tcx> {
    pub tcx: TyCtxt<'tcx>,
    pub span: Span,
}

impl Deref for TyCtxtAt<'tcx> {
    type Target = TyCtxt<'tcx>;
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.tcx
    }
}

#[derive(Copy, Clone)]
pub struct TyCtxtEnsure<'tcx> {
    pub tcx: TyCtxt<'tcx>,
}

impl TyCtxt<'tcx> {
    // 添加注释: 为`TyCtxt`返回一个透明的包装器, 它确保查询被执行, 而不是仅仅返回它们的结果
    /// Returns a transparent wrapper for `TyCtxt`, which ensures queries
    /// are executed instead of just returning their results.
    #[inline(always)]
    pub fn ensure(self) -> TyCtxtEnsure<'tcx> {
        TyCtxtEnsure { tcx: self }
    }

    // 添加注释: 返回`TyCtxt`的透明包装, 该包装使用`span`作为通过其执行的查询的位置.
    /// Returns a transparent wrapper for `TyCtxt` which uses
    /// `span` as the location of queries performed through it.
    #[inline(always)]
    pub fn at(self, span: Span) -> TyCtxtAt<'tcx> {
        TyCtxtAt { tcx: self, span }
    }

    pub fn try_mark_green(self, dep_node: &dep_graph::DepNode) -> bool {
        self.queries.try_mark_green(self, dep_node)
    }
}

macro_rules! query_helper_param_ty {
    (DefId) => { impl IntoQueryParam<DefId> };
    ($K:ty) => { $K };
}

// 添加注释: 进行类型 `as` 操作
// 例: `query_storage!([fatal_cycle , cycle_delay_bug , anon , storage(#ty)][key, ()]);)*`
// `query_storage!([$($modifiers)*][$($K)*, $V])`对应哪个匹配???
macro_rules! query_storage {
    ([][$K:ty, $V:ty]) => {
        <DefaultCacheSelector as CacheSelector<$K, $V>>::Cache
    };
    ([storage($ty:ty) $($rest:tt)*][$K:ty, $V:ty]) => {
        <$ty as CacheSelector<$K, $V>>::Cache
    };
    ([$other:ident $(($($other_args:tt)*))* $(, $($modifiers:tt)*)*][$($args:tt)*]) => {
        query_storage!([$($($modifiers)*)*][$($args)*])
    };
}

macro_rules! define_callbacks {
    (<$tcx:tt>
     $($(#[$attr:meta])*
        [$($modifiers:tt)*] fn $name:ident($($K:tt)*) -> $V:ty,)*) => {

        // 添加注释: HACK(eddyb) 类似于下面的`impl QueryConfig for queries::$name`,
        // 但是使用类型别名而不是关联类型来绕过HRTB规范下的限制, 例如如下:
        // `for<'tcx> fn(...) -> <queries::$name<'tcx> as QueryConfig<TyCtxt<'tcx>>>::Value` 当前未规范为
        // `for<'tcx> fn(...) -> query_values::$name<'tcx>`.

        // 这主要由`rustc_metadata`中的`provide!`使用.

        // HACK(eddyb) this is like the `impl QueryConfig for queries::$name`
        // below, but using type aliases instead of associated types, to bypass
        // the limitations around normalizing under HRTB - for example, this:
        // `for<'tcx> fn(...) -> <queries::$name<'tcx> as QueryConfig<TyCtxt<'tcx>>>::Value`
        // doesn't currently normalize to `for<'tcx> fn(...) -> query_values::$name<'tcx>`.
        // This is primarily used by the `provide!` macro in `rustc_metadata`.
        #[allow(nonstandard_style, unused_lifetimes)]
        pub mod query_keys {
            use super::*;

            // 添加注释: 进行类型别名定义, 使用参数类型进行别名定义
            $(pub type $name<$tcx> = $($K)*;)*
        }
        #[allow(nonstandard_style, unused_lifetimes)]
        pub mod query_values {
            use super::*;

            // 添加注释: 进行类型别名定义, 使用返回类型进行别名定义
            $(pub type $name<$tcx> = $V;)*
        }
        #[allow(nonstandard_style, unused_lifetimes)]
        pub mod query_storage {
            use super::*;

            // 添加注释: 进行类型别名定义, 如果修饰符是storage时, 对应的值是`storage(#ty)`
            // `query_storage!([$($modifiers)*][$($K)*, $V]);)*`应会解析为以下传参形式
            // `query_storage!([fatal_cycle , eval_always ][key, ()]);)*`

            // 添加注释: 当传的`modifiers`中包含有`storage(#ty)`时, 将会匹配到``[storage($ty:ty) $($rest:tt)*][$K:ty, $V:ty],
            // 否则将会匹配到`[][$K:ty, $V:ty]`

            // 添加注释: 如果`modifiers`中包含`storage(#ty)`, 则将使用`<$ty as CacheSelector<$K, $V>>::Cache`进行别名定义
            // 如果`modifiers`中不包含`storage(#ty)`, 则使用`<DefaultCacheSelector as CacheSelector<$K, $V>>::Cache`进行别名定义
            $(pub type $name<$tcx> = query_storage!([$($modifiers)*][$($K)*, $V]);)*
        }
        #[allow(nonstandard_style, unused_lifetimes)]
        pub mod query_stored {
            use super::*;

            // 添加注释: 使用`query_storage`中的类型字段进行别名定义
            $(pub type $name<$tcx> = <query_storage::$name<$tcx> as QueryStorage>::Stored;)*
        }

        #[derive(Default)]
        pub struct QueryCaches<$tcx> {
            $($(#[$attr])* pub $name: QueryCacheStore<query_storage::$name<$tcx>>,)*
        }

        // 添加注释: 为`TyCtxtEnsure`实现方法
        impl TyCtxtEnsure<$tcx> {
            $($(#[$attr])*
            #[inline(always)]
            pub fn $name(self, key: query_helper_param_ty!($($K)*)) {
                let key = key.into_query_param();
                let cached = try_get_cached(self.tcx, &self.tcx.query_caches.$name, &key, |_| {});

                let lookup = match cached {
                    Ok(()) => return,
                    Err(lookup) => lookup,
                };

                self.tcx.queries.$name(self.tcx, DUMMY_SP, key, lookup, QueryMode::Ensure);
            })*
        }

        // 添加注释: 为`TyCtxt`实现方法
        impl TyCtxt<$tcx> {
            $($(#[$attr])*
            #[inline(always)]
            #[must_use]
            pub fn $name(self, key: query_helper_param_ty!($($K)*)) -> query_stored::$name<$tcx>
            {
                self.at(DUMMY_SP).$name(key)
            })*
        }

        // 添加注释: 为`TyCtxtAt`实现方法
        impl TyCtxtAt<$tcx> {
            $($(#[$attr])*
            #[inline(always)]
            pub fn $name(self, key: query_helper_param_ty!($($K)*)) -> query_stored::$name<$tcx>
            {
                let key = key.into_query_param();
                let cached = try_get_cached(self.tcx, &self.tcx.query_caches.$name, &key, |value| {
                    value.clone()
                });

                let lookup = match cached {
                    Ok(value) => return value,
                    Err(lookup) => lookup,
                };

                self.tcx.queries.$name(self.tcx, self.span, key, lookup, QueryMode::Get).unwrap()
            })*
        }

        pub struct Providers {
            $(pub $name: for<'tcx> fn(
                TyCtxt<'tcx>,
                query_keys::$name<'tcx>,
            ) -> query_values::$name<'tcx>,)*
        }

        impl Default for Providers {
            fn default() -> Self {
                Providers {
                    $($name: |_, key| bug!(
                        "`tcx.{}({:?})` unsupported by its crate; \
                         perhaps the `{}` query was never assigned a provider function",
                        stringify!($name),
                        key,
                        stringify!($name),
                    ),)*
                }
            }
        }

        impl Copy for Providers {}
        impl Clone for Providers {
            fn clone(&self) -> Self { *self }
        }

        // 添加注释: 定义`QueryEngine<'tcx>` trait, 并为其继承`rustc_data_structures::sync::Sync` trait
        pub trait QueryEngine<'tcx>: rustc_data_structures::sync::Sync {
            unsafe fn deadlock(&'tcx self, tcx: TyCtxt<'tcx>, registry: &rustc_rayon_core::Registry);

            fn encode_query_results(
                &'tcx self,
                tcx: TyCtxt<'tcx>,
                encoder: &mut on_disk_cache::CacheEncoder<'a, 'tcx, opaque::FileEncoder>,
                query_result_index: &mut on_disk_cache::EncodedQueryResultIndex,
            ) -> opaque::FileEncodeResult;

            fn exec_cache_promotions(&'tcx self, tcx: TyCtxt<'tcx>);

            fn try_mark_green(&'tcx self, tcx: TyCtxt<'tcx>, dep_node: &dep_graph::DepNode) -> bool;

            fn try_print_query_stack(
                &'tcx self,
                tcx: TyCtxt<'tcx>,
                query: Option<QueryJobId<dep_graph::DepKind>>,
                handler: &Handler,
                num_frames: Option<usize>,
            ) -> usize;

            $($(#[$attr])*
            fn $name(
                &'tcx self,
                tcx: TyCtxt<$tcx>,
                span: Span,
                key: query_keys::$name<$tcx>,
                lookup: QueryLookup,
                mode: QueryMode,
            ) -> Option<query_stored::$name<$tcx>>;)*
        }
    };
}

// 添加注释: 这些查询中的每个字段都对应于`Provideers`中的每个函数指针字段, 用于请求该类型
// 的值, 以及`tcx: TyCtxt(和`tcx.at(span)`)`上的一个方法, 用于在一个记忆并进行深度图
// 跟踪的方式, 环绕驱动程序创建的实际`Providers`(使用几个`rustc_*`板条箱).
// Each of these queries corresponds to a function pointer field in the
// `Providers` struct for requesting a value of that type, and a method
// on `tcx: TyCtxt` (and `tcx.at(span)`) for doing that request in a way
// which memoizes and does dep-graph tracking, wrapping around the actual
// `Providers` that the driver creates (using several `rustc_*` crates).
//
// 添加注释: 每个查询的结果类型必须实现`Clone`, 另外还有`ty::query::values::Value`,
// 如果查询导致查询循环, 它会产生一个适当的占位符(错误)值. 标记为`fatal_cycle`的查询不需要
// 后一种实现, 因为它们会在查询周期中引发致命错误.
// The result type of each query must implement `Clone`, and additionally
// `ty::query::values::Value`, which produces an appropriate placeholder
// (error) value if the query resulted in a query cycle.
// Queries marked with `fatal_cycle` do not need the latter implementation,
// as they will raise an fatal error on query cycles instead.

rustc_query_append! { [define_callbacks!][<'tcx>] }

mod sealed {
    use super::{DefId, LocalDefId};

    /// An analogue of the `Into` trait that's intended only for query paramaters.
    ///
    /// This exists to allow queries to accept either `DefId` or `LocalDefId` while requiring that the
    /// user call `to_def_id` to convert between them everywhere else.
    pub trait IntoQueryParam<P> {
        fn into_query_param(self) -> P;
    }

    impl<P> IntoQueryParam<P> for P {
        #[inline(always)]
        fn into_query_param(self) -> P {
            self
        }
    }

    impl IntoQueryParam<DefId> for LocalDefId {
        #[inline(always)]
        fn into_query_param(self) -> DefId {
            self.to_def_id()
        }
    }
}

use sealed::IntoQueryParam;

impl TyCtxt<'tcx> {
    pub fn def_kind(self, def_id: impl IntoQueryParam<DefId>) -> DefKind {
        let def_id = def_id.into_query_param();
        self.opt_def_kind(def_id)
            .unwrap_or_else(|| bug!("def_kind: unsupported node: {:?}", def_id))
    }
}

impl TyCtxtAt<'tcx> {
    pub fn def_kind(self, def_id: impl IntoQueryParam<DefId>) -> DefKind {
        let def_id = def_id.into_query_param();
        self.opt_def_kind(def_id)
            .unwrap_or_else(|| bug!("def_kind: unsupported node: {:?}", def_id))
    }
}
