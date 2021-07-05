// 添加注释: 用于了解所有上游crate(rlibs/dylibs/on my)的依赖格式的类型定义.
//! Type definitions for learning about the dependency formats of all upstream
//! crates (rlibs/dylibs/oh my).
//!
// 添加注释: 有关所有详细信息, 请参阅`dependency_formats`查询的提供者
//! For all the gory details, see the provider of the `dependency_formats`
//! query.

use rustc_session::config::CrateType;

// 添加注释: 特定crate类型的依赖项列表
/// A list of dependencies for a certain crate type.
///
// 添加注释: 该向量的长度与使用的外部crate的数量相同. 如果crate不需要链接(它是在另一个dylib中静态找到的),
// 则值为None, 如果需要链接为`kind`(静态或动态), 则值为Some(kind)
/// The length of this vector is the same as the number of external crates used.
/// The value is None if the crate does not need to be linked (it was found
/// statically in another dylib), or Some(kind) if it needs to be linked as
/// `kind` (either static or dynamic).
pub type DependencyList = Vec<Linkage>;

// 添加注释: 特定输出风格的所有必需依赖项的映射.
/// A mapping of all required dependencies for a particular flavor of output.
///
// 添加注释: 这是tcx本地的, 通常与一个会话相关.
/// This is local to the tcx, and is generally relevant to one session.
pub type Dependencies = Vec<(CrateType, DependencyList)>;

#[derive(Copy, Clone, PartialEq, Debug, HashStable, Encodable, Decodable)]
pub enum Linkage {
    NotLinked,
    IncludedFromDylib,
    Static,
    Dynamic,
}
