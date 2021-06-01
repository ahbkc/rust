use measureme::{StringComponent, StringId};
use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::profiling::SelfProfiler;
use rustc_hir::def_id::{CrateNum, DefId, DefIndex, LocalDefId, CRATE_DEF_INDEX, LOCAL_CRATE};
use rustc_hir::definitions::DefPathData;
use rustc_middle::ty::{TyCtxt, WithOptConstParam};
use rustc_query_system::query::{QueryCache, QueryCacheStore};
use std::fmt::Debug;
use std::io::Write;

struct QueryKeyStringCache {
    def_id_cache: FxHashMap<DefId, StringId>,
}

impl QueryKeyStringCache {
    fn new() -> QueryKeyStringCache {
        QueryKeyStringCache { def_id_cache: Default::default() }
    }
}

struct QueryKeyStringBuilder<'p, 'c, 'tcx> {
    profiler: &'p SelfProfiler,
    tcx: TyCtxt<'tcx>,
    string_cache: &'c mut QueryKeyStringCache,
}

impl<'p, 'c, 'tcx> QueryKeyStringBuilder<'p, 'c, 'tcx> {
    fn new(
        profiler: &'p SelfProfiler,
        tcx: TyCtxt<'tcx>,
        string_cache: &'c mut QueryKeyStringCache,
    ) -> QueryKeyStringBuilder<'p, 'c, 'tcx> {
        QueryKeyStringBuilder { profiler, tcx, string_cache }
    }

    // The current implementation is rather crude. In the future it might be a
    // good idea to base this on `ty::print` in order to get nicer and more
    // efficient query keys.
    fn def_id_to_string_id(&mut self, def_id: DefId) -> StringId {
        if let Some(&string_id) = self.string_cache.def_id_cache.get(&def_id) {
            return string_id;
        }

        let def_key = self.tcx.def_key(def_id);

        let (parent_string_id, start_index) = match def_key.parent {
            Some(parent_index) => {
                let parent_def_id = DefId { index: parent_index, krate: def_id.krate };

                (self.def_id_to_string_id(parent_def_id), 0)
            }
            None => (StringId::INVALID, 2),
        };

        let dis_buffer = &mut [0u8; 16];
        let crate_name;
        let other_name;
        let name;
        let dis;
        let end_index;

        match def_key.disambiguated_data.data {
            DefPathData::CrateRoot => {
                crate_name = self.tcx.original_crate_name(def_id.krate).as_str();
                name = &*crate_name;
                dis = "";
                end_index = 3;
            }
            other => {
                other_name = other.to_string();
                name = other_name.as_str();
                if def_key.disambiguated_data.disambiguator == 0 {
                    dis = "";
                    end_index = 3;
                } else {
                    write!(&mut dis_buffer[..], "[{}]", def_key.disambiguated_data.disambiguator)
                        .unwrap();
                    let end_of_dis = dis_buffer.iter().position(|&c| c == b']').unwrap();
                    dis = std::str::from_utf8(&dis_buffer[..end_of_dis + 1]).unwrap();
                    end_index = 4;
                }
            }
        }

        let components = [
            StringComponent::Ref(parent_string_id),
            StringComponent::Value("::"),
            StringComponent::Value(name),
            StringComponent::Value(dis),
        ];

        let string_id = self.profiler.alloc_string(&components[start_index..end_index]);

        self.string_cache.def_id_cache.insert(def_id, string_id);

        string_id
    }
}

trait IntoSelfProfilingString {
    fn to_self_profile_string(&self, builder: &mut QueryKeyStringBuilder<'_, '_, '_>) -> StringId;
}

// The default implementation of `IntoSelfProfilingString` just uses `Debug`
// which is slow and causes lots of duplication of string data.
// The specialized impls below take care of making the `DefId` case more
// efficient.
impl<T: Debug> IntoSelfProfilingString for T {
    default fn to_self_profile_string(
        &self,
        builder: &mut QueryKeyStringBuilder<'_, '_, '_>,
    ) -> StringId {
        let s = format!("{:?}", self);
        builder.profiler.alloc_string(&s[..])
    }
}

impl<T: SpecIntoSelfProfilingString> IntoSelfProfilingString for T {
    fn to_self_profile_string(&self, builder: &mut QueryKeyStringBuilder<'_, '_, '_>) -> StringId {
        self.spec_to_self_profile_string(builder)
    }
}

#[rustc_specialization_trait]
trait SpecIntoSelfProfilingString: Debug {
    fn spec_to_self_profile_string(
        &self,
        builder: &mut QueryKeyStringBuilder<'_, '_, '_>,
    ) -> StringId;
}

impl SpecIntoSelfProfilingString for DefId {
    fn spec_to_self_profile_string(
        &self,
        builder: &mut QueryKeyStringBuilder<'_, '_, '_>,
    ) -> StringId {
        builder.def_id_to_string_id(*self)
    }
}

impl SpecIntoSelfProfilingString for CrateNum {
    fn spec_to_self_profile_string(
        &self,
        builder: &mut QueryKeyStringBuilder<'_, '_, '_>,
    ) -> StringId {
        builder.def_id_to_string_id(DefId { krate: *self, index: CRATE_DEF_INDEX })
    }
}

impl SpecIntoSelfProfilingString for DefIndex {
    fn spec_to_self_profile_string(
        &self,
        builder: &mut QueryKeyStringBuilder<'_, '_, '_>,
    ) -> StringId {
        builder.def_id_to_string_id(DefId { krate: LOCAL_CRATE, index: *self })
    }
}

impl SpecIntoSelfProfilingString for LocalDefId {
    fn spec_to_self_profile_string(
        &self,
        builder: &mut QueryKeyStringBuilder<'_, '_, '_>,
    ) -> StringId {
        builder.def_id_to_string_id(DefId { krate: LOCAL_CRATE, index: self.local_def_index })
    }
}

impl<T: SpecIntoSelfProfilingString> SpecIntoSelfProfilingString for WithOptConstParam<T> {
    fn spec_to_self_profile_string(
        &self,
        builder: &mut QueryKeyStringBuilder<'_, '_, '_>,
    ) -> StringId {
        // We print `WithOptConstParam` values as tuples to make them shorter
        // and more readable, without losing information:
        //
        // "WithOptConstParam { did: foo::bar, const_param_did: Some(foo::baz) }"
        // becomes "(foo::bar, foo::baz)" and
        // "WithOptConstParam { did: foo::bar, const_param_did: None }"
        // becomes "(foo::bar, _)".

        let did = StringComponent::Ref(self.did.to_self_profile_string(builder));

        let const_param_did = if let Some(const_param_did) = self.const_param_did {
            let const_param_did = builder.def_id_to_string_id(const_param_did);
            StringComponent::Ref(const_param_did)
        } else {
            StringComponent::Value("_")
        };

        let components = [
            StringComponent::Value("("),
            did,
            StringComponent::Value(", "),
            const_param_did,
            StringComponent::Value(")"),
        ];

        builder.profiler.alloc_string(&components[..])
    }
}

impl<T0, T1> SpecIntoSelfProfilingString for (T0, T1)
where
    T0: SpecIntoSelfProfilingString,
    T1: SpecIntoSelfProfilingString,
{
    fn spec_to_self_profile_string(
        &self,
        builder: &mut QueryKeyStringBuilder<'_, '_, '_>,
    ) -> StringId {
        let val0 = self.0.to_self_profile_string(builder);
        let val1 = self.1.to_self_profile_string(builder);

        let components = &[
            StringComponent::Value("("),
            StringComponent::Ref(val0),
            StringComponent::Value(","),
            StringComponent::Ref(val1),
            StringComponent::Value(")"),
        ];

        builder.profiler.alloc_string(components)
    }
}

// 添加注释: 为单个查询缓存分配自配置查询字符串. 这个方法是从`alloc_self_profile_query_strings`调用的,
// 它通过宏魔法知道所有的查询.
/// Allocate the self-profiling query strings for a single query cache. This
/// method is called from `alloc_self_profile_query_strings` which knows all
/// the queries via macro magic.
fn alloc_self_profile_query_strings_for_query_cache<'tcx, C>(
    tcx: TyCtxt<'tcx>,
    query_name: &'static str,
    query_cache: &QueryCacheStore<C>,
    string_cache: &mut QueryKeyStringCache,
) where
    C: QueryCache,
    C::Key: Debug + Clone,
{
    tcx.prof.with_profiler(|profiler| {
        // 添加注释: 返回构建的`EventIdBuilder`类型值
        let event_id_builder = profiler.event_id_builder();

        // 添加注释: 遍历整个查询缓存并分配适当的字符串表示. 每个缓存条目由其`dep_node_index`
        // 唯一标识
        // Walk the entire query cache and allocate the appropriate
        // string representations. Each cache entry is uniquely
        // identified by its dep_node_index.
        if profiler.query_key_recording_enabled() {
            let mut query_string_builder = QueryKeyStringBuilder::new(profiler, tcx, string_cache);

            let query_name = profiler.get_or_alloc_cached_string(query_name);

            // 添加注释: 由于构建查询键的字符串表示可能需要调用查询本身, 因此我们无法在这样做时保持查询缓存锁定.
            // 相反, 我们复制了`(query_key, dep_node_index)`对并再次释放锁.
            // Since building the string representation of query keys might
            // need to invoke queries itself, we cannot keep the query caches
            // locked while doing so. Instead we copy out the
            // `(query_key, dep_node_index)` pairs and release the lock again.
            let mut query_keys_and_indices = Vec::new();
            query_cache.iter_results(&mut |k, _, i| query_keys_and_indices.push((k.clone(), i)));

            // 添加注释: 现在实际分配字符串. 如果分配字符串在查询缓存中生成新条目, 我们会错过它们, 但我们
            // 实际上并不关心.
            // Now actually allocate the strings. If allocating the strings
            // generates new entries in the query cache, we'll miss them but
            // we don't actually care.
            for (query_key, dep_node_index) in query_keys_and_indices {
                // Translate the DepNodeIndex into a QueryInvocationId
                let query_invocation_id = dep_node_index.into();

                // 添加注释: 创建查询键的字符串版本
                // Create the string version of the query-key
                let query_key = query_key.to_self_profile_string(&mut query_string_builder);
                let event_id = event_id_builder.from_label_and_arg(query_name, query_key);

                // 添加注释: 批量执行此操作可能是个好主意:
                // Doing this in bulk might be a good idea:
                profiler.map_query_invocation_id_to_string(
                    query_invocation_id,
                    event_id.to_string_id(),
                );
            }
        } else {
            // 添加注释: 在这个分支中, 我们不分配查询键
            // In this branch we don't allocate query keys
            let query_name = profiler.get_or_alloc_cached_string(query_name);
            let event_id = event_id_builder.from_label(query_name).to_string_id();

            let mut query_invocation_ids = Vec::new();
            query_cache.iter_results(&mut |_, _, i| {
                query_invocation_ids.push(i.into());
            });

            profiler.bulk_map_query_invocation_id_to_single_string(
                query_invocation_ids.into_iter(),
                event_id,
            );
        }
    });
}

// 添加注释: 查询引擎生成的所有自分析事件都使用虚拟的`StringId`作为其`event_id`.
// 此方法使所有这些虚拟的`StringId`指向实现的字符串.
/// All self-profiling events generated by the query engine use
/// virtual `StringId`s for their `event_id`. This method makes all
/// those virtual `StringId`s point to actual strings.
///
// 添加注释: 如果我们只记录摘要数据, ID将只指向查询名称. 如果我们也记录查询键,
// 我们在这里分配相应的字符串
/// If we are recording only summary data, the ids will point to
/// just the query names. If we are recording query keys too, we
/// allocate the corresponding strings here.
pub fn alloc_self_profile_query_strings(tcx: TyCtxt<'tcx>) {
    if !tcx.prof.enabled() {
        return;
    }

    let mut string_cache = QueryKeyStringCache::new();

    macro_rules! alloc_once {
        (<$tcx:tt>
            $($(#[$attr:meta])* [$($modifiers:tt)*] fn $name:ident($K:ty) -> $V:ty,)*
        ) => {
            $({
                alloc_self_profile_query_strings_for_query_cache(
                    tcx,
                    stringify!($name),
                    &tcx.query_caches.$name,
                    &mut string_cache,
                );
            })*
        }
    }

    rustc_query_append! { [alloc_once!][<'tcx>] }
}
