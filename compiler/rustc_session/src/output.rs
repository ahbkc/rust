//! Related to out filenames of compilation (e.g. save analysis, binaries).
use crate::config::{CrateType, Input, OutputFilenames, OutputType};
use crate::Session;
use rustc_ast as ast;
use rustc_span::symbol::sym;
use rustc_span::Span;
use std::path::{Path, PathBuf};

pub fn out_filename(
    sess: &Session,
    crate_type: CrateType,
    outputs: &OutputFilenames,
    crate_name: &str,
) -> PathBuf {
    let default_filename = filename_for_input(sess, crate_type, crate_name, outputs);
    let out_filename = outputs
        .outputs
        .get(&OutputType::Exe)
        .and_then(|s| s.to_owned())
        .or_else(|| outputs.single_output_file.clone())
        .unwrap_or(default_filename);

    check_file_is_writeable(&out_filename, sess);

    out_filename
}

/// Make sure files are writeable.  Mac, FreeBSD, and Windows system linkers
/// check this already -- however, the Linux linker will happily overwrite a
/// read-only file.  We should be consistent.
pub fn check_file_is_writeable(file: &Path, sess: &Session) {
    if !is_writeable(file) {
        sess.fatal(&format!(
            "output file {} is not writeable -- check its \
                            permissions",
            file.display()
        ));
    }
}

fn is_writeable(p: &Path) -> bool {
    match p.metadata() {
        Err(..) => true,
        Ok(m) => !m.permissions().readonly(),
    }
}

pub fn find_crate_name(sess: &Session, attrs: &[ast::Attribute], input: &Input) -> String {
    // 添加注释: 一个用于验证的闭包
    let validate = |s: String, span: Option<Span>| {
        validate_crate_name(sess, &s, span);
        s
    };

    // 添加注释: 100%的时间查看属性以确保将该属性标记为已使用. 但是, 执行完此操作后, 我们仍然会优先使用
    // 命令行中的`--crate-name`优先于在`#[crate_name]`属性中找到的crate-name
    // 如果我们找到两者, 则可以确保以后也相同
    // Look in attributes 100% of the time to make sure the attribute is marked
    // as used. After doing this, however, we still prioritize a crate name from
    // the command line over one found in the #[crate_name] attribute. If we
    // find both we ensure that they're the same later on as well.
    let attr_crate_name =
        sess.find_by_name(attrs, sym::crate_name).and_then(|at| at.value_str().map(|s| (at, s)));

    // 添加注释: 如果在命令行中输入了`--crate-name`参数
    if let Some(ref s) = sess.opts.crate_name {
        // 添加注释: 判断`--crate-name`和`#[crate_name]`是否相等
        if let Some((attr, name)) = attr_crate_name {
            if name.as_str() != *s {
                let msg = format!(
                    "`--crate-name` and `#[crate_name]` are \
                                   required to match, but `{}` != `{}`",
                    s, name
                );
                sess.span_err(attr.span, &msg);
            }
        }
        return validate(s.clone(), None);
    }

    // 添加注释: 如果从`attrs`中找到了attr_crate_name
    if let Some((attr, s)) = attr_crate_name {
        return validate(s.to_string(), Some(attr.span));
    }
    if let Input::File(ref path) = *input {
        if let Some(s) = path.file_stem().and_then(|s| s.to_str()) {
            if s.starts_with('-') {
                let msg = format!(
                    "crate names cannot start with a `-`, but \
                                   `{}` has a leading hyphen",
                    s
                );
                sess.err(&msg);
            } else {
                return validate(s.replace("-", "_"), None);
            }
        }
    }

    "rust_out".to_string()
}

// 添加注释: 验证`crate_name`
pub fn validate_crate_name(sess: &Session, s: &str, sp: Option<Span>) {
    let mut err_count = 0;
    {
        let mut say = |s: &str| {
            match sp {
                Some(sp) => sess.span_err(sp, s),
                None => sess.err(s),
            }
            err_count += 1;
        };
        if s.is_empty() {
            say("crate name must not be empty");
        }
        for c in s.chars() {
            // 添加注释: [`is_alphabetic()`]如果此char具有`Alphabetic`属性, 则返回true, 用于匹配字母
            // 添加注释: [`is_numeric()`]如果此char具有数字的常规类别之一, 则返回true, 用于匹配数字
            // 添加注释: 如果此`char`满足[`is_alphabetic()`] or [`is_numeric()`], 则返回true, 用于匹配字母和数字
            if c.is_alphanumeric() {
                continue;
            }
            if c == '_' {
                continue;
            }
            say(&format!("invalid character `{}` in crate name: `{}`", c, s));
        }
    }

    if err_count > 0 {
        // 添加注释: 当错误数量大于0, 则调用终止方法
        sess.abort_if_errors();
    }
}

pub fn filename_for_metadata(
    sess: &Session,
    crate_name: &str,
    outputs: &OutputFilenames,
) -> PathBuf {
    let libname = format!("{}{}", crate_name, sess.opts.cg.extra_filename);

    let out_filename = outputs
        .single_output_file
        .clone()
        .unwrap_or_else(|| outputs.out_directory.join(&format!("lib{}.rmeta", libname)));

    check_file_is_writeable(&out_filename, sess);

    out_filename
}

pub fn filename_for_input(
    sess: &Session,
    crate_type: CrateType,
    crate_name: &str,
    outputs: &OutputFilenames,
) -> PathBuf {
    let libname = format!("{}{}", crate_name, sess.opts.cg.extra_filename);

    match crate_type {
        CrateType::Rlib => outputs.out_directory.join(&format!("lib{}.rlib", libname)),
        CrateType::Cdylib | CrateType::ProcMacro | CrateType::Dylib => {
            let (prefix, suffix) = (&sess.target.dll_prefix, &sess.target.dll_suffix);
            outputs.out_directory.join(&format!("{}{}{}", prefix, libname, suffix))
        }
        CrateType::Staticlib => {
            let (prefix, suffix) = (&sess.target.staticlib_prefix, &sess.target.staticlib_suffix);
            outputs.out_directory.join(&format!("{}{}{}", prefix, libname, suffix))
        }
        CrateType::Executable => {
            let suffix = &sess.target.exe_suffix;
            let out_filename = outputs.path(OutputType::Exe);
            if suffix.is_empty() { out_filename } else { out_filename.with_extension(&suffix[1..]) }
        }
    }
}

/// Returns default crate type for target
///
/// Default crate type is used when crate type isn't provided neither
/// through cmd line arguments nor through crate attributes
///
/// It is CrateType::Executable for all platforms but iOS as there is no
/// way to run iOS binaries anyway without jailbreaking and
/// interaction with Rust code through static library is the only
/// option for now
pub fn default_output_for_target(sess: &Session) -> CrateType {
    if !sess.target.executables { CrateType::Staticlib } else { CrateType::Executable }
}

// 添加注释: 检查目标是否支持crate_type作为输出
/// Checks if target supports crate_type as output
pub fn invalid_output_for_target(sess: &Session, crate_type: CrateType) -> bool {
    match crate_type {
        CrateType::Cdylib | CrateType::Dylib | CrateType::ProcMacro => {
            if !sess.target.dynamic_linking {
                return true;
            }
            if sess.crt_static(Some(crate_type)) && !sess.target.crt_static_allows_dylibs {
                return true;
            }
        }
        _ => {}
    }
    if sess.target.only_cdylib {
        match crate_type {
            CrateType::ProcMacro | CrateType::Dylib => return true,
            _ => {}
        }
    }
    if !sess.target.executables && crate_type == CrateType::Executable {
        return true;
    }

    false
}
