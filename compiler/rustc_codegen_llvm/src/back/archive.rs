// 添加注释: 处理静态档案的辅助类
//! A helper class for dealing with static archives

use std::ffi::{CStr, CString};
use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::ptr;
use std::str;

use crate::llvm::archive_ro::{ArchiveRO, Child};
use crate::llvm::{self, ArchiveKind};
use rustc_codegen_ssa::back::archive::{find_library, ArchiveBuilder};
use rustc_codegen_ssa::{looks_like_rust_object_file, METADATA_FILENAME};
use rustc_session::Session;
use rustc_span::symbol::Symbol;

struct ArchiveConfig<'a> {
    pub sess: &'a Session,
    pub dst: PathBuf,
    pub src: Option<PathBuf>,
    pub lib_search_paths: Vec<PathBuf>,
}

// 添加注释: 将许多文件添加到存档的助手.
/// Helper for adding many files to an archive.
#[must_use = "must call build() to finish building the archive"]
pub struct LlvmArchiveBuilder<'a> {
    config: ArchiveConfig<'a>,
    removals: Vec<String>,
    additions: Vec<Addition>,
    should_update_symbols: bool,
    src_archive: Option<Option<ArchiveRO>>,
}

enum Addition {
    File { path: PathBuf, name_in_archive: String },
    Archive { path: PathBuf, archive: ArchiveRO, skip: Box<dyn FnMut(&str) -> bool> },
}

impl Addition {
    fn path(&self) -> &Path {
        match self {
            Addition::File { path, .. } | Addition::Archive { path, .. } => path,
        }
    }
}

fn is_relevant_child(c: &Child<'_>) -> bool {
    match c.name() {
        Some(name) => !name.contains("SYMDEF"),
        None => false,
    }
}

// 添加注释: 创建将要修改的文档
fn archive_config<'a>(sess: &'a Session, output: &Path, input: Option<&Path>) -> ArchiveConfig<'a> {
    use rustc_codegen_ssa::back::link::archive_search_paths;
    ArchiveConfig {
        sess,
        dst: output.to_path_buf(),
        src: input.map(|p| p.to_path_buf()),
        lib_search_paths: archive_search_paths(sess),
    }
}

impl<'a> ArchiveBuilder<'a> for LlvmArchiveBuilder<'a> {
    // 添加注释: 创建一个新的静态存档, 准备修改`config`指定的存档.
    /// Creates a new static archive, ready for modifying the archive specified
    /// by `config`.
    fn new(sess: &'a Session, output: &Path, input: Option<&Path>) -> LlvmArchiveBuilder<'a> {
        let config = archive_config(sess, output, input);
        LlvmArchiveBuilder {
            config,
            removals: Vec::new(),
            additions: Vec::new(),
            should_update_symbols: false,
            src_archive: None,
        }
    }

    // 添加注释: 从些存档中删除文件
    /// Removes a file from this archive
    fn remove_file(&mut self, file: &str) {
        self.removals.push(file.to_string());
    }

    // 添加注释: 列出此存档中的所有文件
    /// Lists all files in an archive
    fn src_files(&mut self) -> Vec<String> {
        if self.src_archive().is_none() {
            return Vec::new();
        }

        let archive = self.src_archive.as_ref().unwrap().as_ref().unwrap();

        archive
            .iter()
            .filter_map(|child| child.ok())
            .filter(is_relevant_child)
            .filter_map(|child| child.name())
            .filter(|name| !self.removals.iter().any(|x| x == name))
            .map(|name| name.to_owned())
            .collect()
    }

    // 添加注释: 将本机库的所有内容添加到此文档中. 这将在相关位置搜索名为`name`的库.
    /// Adds all of the contents of a native library to this archive. This will
    /// search in the relevant locations for a library named `name`.
    fn add_native_library(&mut self, name: Symbol, verbatim: bool) {
        let location =
            find_library(name, verbatim, &self.config.lib_search_paths, self.config.sess);
        self.add_archive(&location, |_| false).unwrap_or_else(|e| {
            self.config.sess.fatal(&format!(
                "failed to add native library {}: {}",
                location.to_string_lossy(),
                e
            ));
        });
    }

    // 添加注释: 将指定路径处的rlib的所有内容添加到此存档中
    /// Adds all of the contents of the rlib at the specified path to this
    /// archive.
    ///
    // 添加注释: 这会忽略从rlib添加字节码, 如果启用了LTO, 则也不会添加目标文件.
    /// This ignores adding the bytecode from the rlib, and if LTO is enabled
    /// then the object file also isn't added.
    fn add_rlib(
        &mut self,
        rlib: &Path,
        name: &str,
        lto: bool,
        skip_objects: bool,
    ) -> io::Result<()> {
        // Ignoring obj file starting with the crate name
        // as simple comparison is not enough - there
        // might be also an extra name suffix
        let obj_start = name.to_owned();

        self.add_archive(rlib, move |fname: &str| {
            // Ignore metadata files, no matter the name.
            if fname == METADATA_FILENAME {
                return true;
            }

            // Don't include Rust objects if LTO is enabled
            if lto && looks_like_rust_object_file(fname) {
                return true;
            }

            // Otherwise if this is *not* a rust object and we're skipping
            // objects then skip this file
            if skip_objects && (!fname.starts_with(&obj_start) || !fname.ends_with(".o")) {
                return true;
            }

            // ok, don't skip this
            false
        })
    }

    // 添加注释: 将任意文件添加到此存档.
    /// Adds an arbitrary file to this archive
    fn add_file(&mut self, file: &Path) {
        let name = file.file_name().unwrap().to_str().unwrap();
        self.additions
            .push(Addition::File { path: file.to_path_buf(), name_in_archive: name.to_owned() });
    }

    // 添加注释: 指示下一次调用`build`应该更新存档中的所有符号(相当于在它上面运行`ar s`)
    /// Indicate that the next call to `build` should update all symbols in
    /// the archive (equivalent to running 'ar s' over it).
    fn update_symbols(&mut self) {
        self.should_update_symbols = true;
    }

    // 添加注释: 将提供的文件, rlib和native库合并到一个`Archive`中
    /// Combine the provided files, rlibs, and native libraries into a single
    /// `Archive`.
    fn build(mut self) {
        let kind = self.llvm_archive_kind().unwrap_or_else(|kind| {
            self.config.sess.fatal(&format!("Don't know how to build archive of type: {}", kind))
        });

        if let Err(e) = self.build_with_llvm(kind) {
            self.config.sess.fatal(&format!("failed to build archive: {}", e));
        }
    }
}

impl<'a> LlvmArchiveBuilder<'a> {
    fn src_archive(&mut self) -> Option<&ArchiveRO> {
        if let Some(ref a) = self.src_archive {
            return a.as_ref();
        }
        let src = self.config.src.as_ref()?;
        self.src_archive = Some(ArchiveRO::open(src).ok());
        self.src_archive.as_ref().unwrap().as_ref()
    }

    fn add_archive<F>(&mut self, archive: &Path, skip: F) -> io::Result<()>
    where
        F: FnMut(&str) -> bool + 'static,
    {
        let archive_ro = match ArchiveRO::open(archive) {
            Ok(ar) => ar,
            Err(e) => return Err(io::Error::new(io::ErrorKind::Other, e)),
        };
        if self.additions.iter().any(|ar| ar.path() == archive) {
            return Ok(());
        }
        self.additions.push(Addition::Archive {
            path: archive.to_path_buf(),
            archive: archive_ro,
            skip: Box::new(skip),
        });
        Ok(())
    }

    fn llvm_archive_kind(&self) -> Result<ArchiveKind, &str> {
        let kind = &*self.config.sess.target.archive_format;
        kind.parse().map_err(|_| kind)
    }

    fn build_with_llvm(&mut self, kind: ArchiveKind) -> io::Result<()> {
        let removals = mem::take(&mut self.removals);
        let mut additions = mem::take(&mut self.additions);
        let mut strings = Vec::new();
        let mut members = Vec::new();

        let dst = CString::new(self.config.dst.to_str().unwrap())?;
        let should_update_symbols = self.should_update_symbols;

        unsafe {
            if let Some(archive) = self.src_archive() {
                for child in archive.iter() {
                    let child = child.map_err(string_to_io_error)?;
                    let child_name = match child.name() {
                        Some(s) => s,
                        None => continue,
                    };
                    if removals.iter().any(|r| r == child_name) {
                        continue;
                    }

                    let name = CString::new(child_name)?;
                    members.push(llvm::LLVMRustArchiveMemberNew(
                        ptr::null(),
                        name.as_ptr(),
                        Some(child.raw),
                    ));
                    strings.push(name);
                }
            }
            for addition in &mut additions {
                match addition {
                    Addition::File { path, name_in_archive } => {
                        let path = CString::new(path.to_str().unwrap())?;
                        let name = CString::new(name_in_archive.clone())?;
                        members.push(llvm::LLVMRustArchiveMemberNew(
                            path.as_ptr(),
                            name.as_ptr(),
                            None,
                        ));
                        strings.push(path);
                        strings.push(name);
                    }
                    Addition::Archive { archive, skip, .. } => {
                        for child in archive.iter() {
                            let child = child.map_err(string_to_io_error)?;
                            if !is_relevant_child(&child) {
                                continue;
                            }
                            let child_name = child.name().unwrap();
                            if skip(child_name) {
                                continue;
                            }

                            // It appears that LLVM's archive writer is a little
                            // buggy if the name we pass down isn't just the
                            // filename component, so chop that off here and
                            // pass it in.
                            //
                            // See LLVM bug 25877 for more info.
                            let child_name =
                                Path::new(child_name).file_name().unwrap().to_str().unwrap();
                            let name = CString::new(child_name)?;
                            let m = llvm::LLVMRustArchiveMemberNew(
                                ptr::null(),
                                name.as_ptr(),
                                Some(child.raw),
                            );
                            members.push(m);
                            strings.push(name);
                        }
                    }
                }
            }

            let r = llvm::LLVMRustWriteArchive(
                dst.as_ptr(),
                members.len() as libc::size_t,
                members.as_ptr() as *const &_,
                should_update_symbols,
                kind,
            );
            let ret = if r.into_result().is_err() {
                let err = llvm::LLVMRustGetLastError();
                let msg = if err.is_null() {
                    "failed to write archive".into()
                } else {
                    String::from_utf8_lossy(CStr::from_ptr(err).to_bytes())
                };
                Err(io::Error::new(io::ErrorKind::Other, msg))
            } else {
                Ok(())
            };
            for member in members {
                llvm::LLVMRustArchiveMemberFree(member);
            }
            ret
        }
    }
}

fn string_to_io_error(s: String) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("bad archive: {}", s))
}
