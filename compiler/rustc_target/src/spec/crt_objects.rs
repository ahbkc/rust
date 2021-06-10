// 添加注释: Object files为基本运行时工具提供支持, 并在链接开始和结束时添加到生成的二进制文件中.
//! Object files providing support for basic runtime facilities and added to the produced binaries
//! at the start and at the end of linking.
//!
// 添加注释: 流行工具链的CRT对象表.
//! Table of CRT objects for popular toolchains.
// 添加注释: `crtx`通常与lic一起发布, 而`begin/end`与gcc一起发布.
//! The `crtx` ones are generally distributed with libc and the `begin/end` ones with gcc.
// 添加注释: 更多详情查看<https://dev.gentoo.org/~vapier/crt.txt>
//! See <https://dev.gentoo.org/~vapier/crt.txt> for some more details.
//!
//! | Pre-link CRT objects | glibc                  | musl                   | bionic           | mingw             | wasi         |
//! |----------------------|------------------------|------------------------|------------------|-------------------|--------------|
//! | dynamic-nopic-exe    | crt1, crti, crtbegin   | crt1, crti, crtbegin   | crtbegin_dynamic | crt2, crtbegin    | crt1         |
//! | dynamic-pic-exe      | Scrt1, crti, crtbeginS | Scrt1, crti, crtbeginS | crtbegin_dynamic | crt2, crtbegin    | crt1         |
//! | static-nopic-exe     | crt1, crti, crtbeginT  | crt1, crti, crtbegin   | crtbegin_static  | crt2, crtbegin    | crt1         |
//! | static-pic-exe       | rcrt1, crti, crtbeginS | rcrt1, crti, crtbeginS | crtbegin_dynamic | crt2, crtbegin    | crt1         |
//! | dynamic-dylib        | crti, crtbeginS        | crti, crtbeginS        | crtbegin_so      | dllcrt2, crtbegin | -            |
//! | static-dylib (gcc)   | crti, crtbeginT        | crti, crtbeginS        | crtbegin_so      | dllcrt2, crtbegin | -            |
//! | static-dylib (clang) | crti, crtbeginT        | N/A                    | crtbegin_static  | dllcrt2, crtbegin | -            |
//! | wasi-reactor-exe     | N/A                    | N/A                    | N/A              | N/A               | crt1-reactor |
//!
//! | Post-link CRT objects | glibc         | musl          | bionic         | mingw  | wasi |
//! |-----------------------|---------------|---------------|----------------|--------|------|
//! | dynamic-nopic-exe     | crtend, crtn  | crtend, crtn  | crtend_android | crtend | -    |
//! | dynamic-pic-exe       | crtendS, crtn | crtendS, crtn | crtend_android | crtend | -    |
//! | static-nopic-exe      | crtend, crtn  | crtend, crtn  | crtend_android | crtend | -    |
//! | static-pic-exe        | crtendS, crtn | crtendS, crtn | crtend_android | crtend | -    |
//! | dynamic-dylib         | crtendS, crtn | crtendS, crtn | crtend_so      | crtend | -    |
//! | static-dylib (gcc)    | crtend, crtn  | crtendS, crtn | crtend_so      | crtend | -    |
//! | static-dylib (clang)  | crtendS, crtn | N/A           | crtend_so      | crtend | -    |
//!
//! Use cases for rustc linking the CRT objects explicitly:
//!     - rustc needs to add its own Rust-specific objects (mingw is the example)
//!     - gcc wrapper cannot be used for some reason and linker like ld or lld is used directly.
//!     - gcc wrapper pulls wrong CRT objects (e.g. from glibc when we are targeting musl).
//!
// 添加注释: 一般来说, 最好依靠目标的本机工具链来拉取对象.
// 但是, 对于某些目标(musl, mingw), rustc历史上提供了更独立的安装, 不需要用户安装本机目标的工具链.
// 在这种情况下, rustc将对象作为目标Rust工具链的一部分进行分发, 并回退到手动链接它们.
// 与原生工具链不同, rustc目前只在链接期间添加libc的对象, 而不是gcc的. 因此, 在自包含模式下链接时, rustc无法
// 与C++静态库(#36710)链接.
//! In general it is preferable to rely on the target's native toolchain to pull the objects.
//! However, for some targets (musl, mingw) rustc historically provides a more self-contained
//! installation not requiring users to install the native target's toolchain.
//! In that case rustc distributes the objects as a part of the target's Rust toolchain
//! and falls back to linking with them manually.
//! Unlike native toolchains, rustc only currently adds the libc's objects during linking,
//! but not gcc's. As a result rustc cannot link with C++ static libraries (#36710)
//! when linking in self-contained mode.

use crate::spec::LinkOutputKind;
use rustc_serialize::json::{Json, ToJson};
use std::collections::BTreeMap;
use std::str::FromStr;

pub type CrtObjects = BTreeMap<LinkOutputKind, Vec<String>>;

pub(super) fn new(obj_table: &[(LinkOutputKind, &[&str])]) -> CrtObjects {
    obj_table.iter().map(|(z, k)| (*z, k.iter().map(|b| b.to_string()).collect())).collect()
}

pub(super) fn all(obj: &str) -> CrtObjects {
    new(&[
        (LinkOutputKind::DynamicNoPicExe, &[obj]),
        (LinkOutputKind::DynamicPicExe, &[obj]),
        (LinkOutputKind::StaticNoPicExe, &[obj]),
        (LinkOutputKind::StaticPicExe, &[obj]),
        (LinkOutputKind::DynamicDylib, &[obj]),
        (LinkOutputKind::StaticDylib, &[obj]),
    ])
}

pub(super) fn pre_musl_fallback() -> CrtObjects {
    new(&[
        (LinkOutputKind::DynamicNoPicExe, &["crt1.o", "crti.o", "crtbegin.o"]),
        (LinkOutputKind::DynamicPicExe, &["Scrt1.o", "crti.o", "crtbeginS.o"]),
        (LinkOutputKind::StaticNoPicExe, &["crt1.o", "crti.o", "crtbegin.o"]),
        (LinkOutputKind::StaticPicExe, &["rcrt1.o", "crti.o", "crtbeginS.o"]),
        (LinkOutputKind::DynamicDylib, &["crti.o", "crtbeginS.o"]),
        (LinkOutputKind::StaticDylib, &["crti.o", "crtbeginS.o"]),
    ])
}

pub(super) fn post_musl_fallback() -> CrtObjects {
    new(&[
        (LinkOutputKind::DynamicNoPicExe, &["crtend.o", "crtn.o"]),
        (LinkOutputKind::DynamicPicExe, &["crtendS.o", "crtn.o"]),
        (LinkOutputKind::StaticNoPicExe, &["crtend.o", "crtn.o"]),
        (LinkOutputKind::StaticPicExe, &["crtendS.o", "crtn.o"]),
        (LinkOutputKind::DynamicDylib, &["crtendS.o", "crtn.o"]),
        (LinkOutputKind::StaticDylib, &["crtendS.o", "crtn.o"]),
    ])
}

pub(super) fn pre_mingw_fallback() -> CrtObjects {
    new(&[
        (LinkOutputKind::DynamicNoPicExe, &["crt2.o", "rsbegin.o"]),
        (LinkOutputKind::DynamicPicExe, &["crt2.o", "rsbegin.o"]),
        (LinkOutputKind::StaticNoPicExe, &["crt2.o", "rsbegin.o"]),
        (LinkOutputKind::StaticPicExe, &["crt2.o", "rsbegin.o"]),
        (LinkOutputKind::DynamicDylib, &["dllcrt2.o", "rsbegin.o"]),
        (LinkOutputKind::StaticDylib, &["dllcrt2.o", "rsbegin.o"]),
    ])
}

pub(super) fn post_mingw_fallback() -> CrtObjects {
    all("rsend.o")
}

pub(super) fn pre_mingw() -> CrtObjects {
    all("rsbegin.o")
}

pub(super) fn post_mingw() -> CrtObjects {
    all("rsend.o")
}

pub(super) fn pre_wasi_fallback() -> CrtObjects {
    // Use crt1-command.o instead of crt1.o to enable support for new-style
    // commands. See https://reviews.llvm.org/D81689 for more info.
    new(&[
        (LinkOutputKind::DynamicNoPicExe, &["crt1-command.o"]),
        (LinkOutputKind::DynamicPicExe, &["crt1-command.o"]),
        (LinkOutputKind::StaticNoPicExe, &["crt1-command.o"]),
        (LinkOutputKind::StaticPicExe, &["crt1-command.o"]),
        (LinkOutputKind::WasiReactorExe, &["crt1-reactor.o"]),
    ])
}

pub(super) fn post_wasi_fallback() -> CrtObjects {
    new(&[])
}

// 添加注释: 使用哪种逻辑来确定是否回退到"self-contained"模式
/// Which logic to use to determine whether to fall back to the "self-contained" mode or not.
#[derive(Clone, Copy, PartialEq, Hash, Debug)]
pub enum CrtObjectsFallback {
    Musl,
    Mingw,
    Wasm,
}

impl FromStr for CrtObjectsFallback {
    type Err = ();

    fn from_str(s: &str) -> Result<CrtObjectsFallback, ()> {
        Ok(match s {
            "musl" => CrtObjectsFallback::Musl,
            "mingw" => CrtObjectsFallback::Mingw,
            "wasm" => CrtObjectsFallback::Wasm,
            _ => return Err(()),
        })
    }
}

impl ToJson for CrtObjectsFallback {
    fn to_json(&self) -> Json {
        match *self {
            CrtObjectsFallback::Musl => "musl",
            CrtObjectsFallback::Mingw => "mingw",
            CrtObjectsFallback::Wasm => "wasm",
        }
        .to_json()
    }
}
