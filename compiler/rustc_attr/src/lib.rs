// 添加注释: 处理属性和元项的函数和类型
//! Functions and types dealing with attributes and meta items.
//!
// 添加注释: 目前, 大部分逻辑仍然在`rust_ast::attr`中.
//! FIXME(Centril): For now being, much of the logic is still in `rustc_ast::attr`.
// 添加注释: 目标是将`MetaItem`的定义和不需要在`syntax`中的东西移动到这个crate中.
//! The goal is to move the definition of `MetaItem` and things that don't need to be in `syntax`
//! to this crate.

#![cfg_attr(bootstrap, feature(or_patterns))]

#[macro_use]
extern crate rustc_macros;

mod builtin;

pub use builtin::*;
pub use IntType::*;
pub use ReprAttr::*;
pub use StabilityLevel::*;

pub use rustc_ast::attr::*;

pub(crate) use rustc_ast::HashStableContext;
