// 添加注释: 类型大小断言. 第一个参数是一个类型, 第二个参数是它的预期大小.
/// Type size assertion. The first argument is a type and the second argument is its expected size.
#[macro_export]
macro_rules! static_assert_size {
    ($ty:ty, $size:expr) => {
        const _: [(); $size] = [(); ::std::mem::size_of::<$ty>()];
    };
}

#[macro_export]
macro_rules! enum_from_u32 {
    ($(#[$attr:meta])* pub enum $name:ident {
        $($(#[$var_attr:meta])* $variant:ident = $e:expr,)*
    }) => {
        $(#[$attr])*
        pub enum $name {
            $($(#[$var_attr])* $variant = $e),*
        }

        impl $name {
            pub fn from_u32(u: u32) -> Option<$name> {
                $(if u == $name::$variant as u32 {
                    return Some($name::$variant)
                })*
                None
            }
        }
    };
    ($(#[$attr:meta])* pub enum $name:ident {
        $($(#[$var_attr:meta])* $variant:ident,)*
    }) => {
        $(#[$attr])*
        pub enum $name {
            $($(#[$var_attr])* $variant,)*
        }

        impl $name {
            pub fn from_u32(u: u32) -> Option<$name> {
                $(if u == $name::$variant as u32 {
                    return Some($name::$variant)
                })*
                None
            }
        }
    }
}
