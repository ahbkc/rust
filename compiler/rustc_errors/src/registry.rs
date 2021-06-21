use rustc_data_structures::fx::FxHashMap;

#[derive(Debug)]
pub struct InvalidErrorCode;

#[derive(Clone)]
pub struct Registry {
    long_descriptions: FxHashMap<&'static str, Option<&'static str>>,
}

impl Registry {
    pub fn new(long_descriptions: &[(&'static str, Option<&'static str>)]) -> Registry {
        Registry { long_descriptions: long_descriptions.iter().copied().collect() }
    }

    // 添加注释: 如果请求的代码在注册表中不存在, 则返回`InvalidErrorCode`. 否则, 返回一个`Option`,
    // 其中`None`表示错误码有效但没有扩展信息.
    /// Returns `InvalidErrorCode` if the code requested does not exist in the
    /// registry. Otherwise, returns an `Option` where `None` means the error
    /// code is valid but has no extended information.
    pub fn try_find_description(
        &self,
        code: &str,
    ) -> Result<Option<&'static str>, InvalidErrorCode> {
        self.long_descriptions.get(code).copied().ok_or(InvalidErrorCode)
    }
}
