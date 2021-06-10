// 添加注释: 这是在增加大小之前需要留在堆栈上的字节数. 它必须至少与任何不调用`ensure_sufficient_stack`的代码
// 所需的堆栈一样大.
// This is the amount of bytes that need to be left on the stack before increasing the size.
// It must be at least as large as the stack required by any code that does not call
// `ensure_sufficient_stack`.
const RED_ZONE: usize = 100 * 1024; // 100k

// 添加注释: 从那时起, 只有被压入的第一个堆栈呈指数增长(2^n * STACK_PER_RECURSION).
// 此标志具有与性能相关的特性. 不要设置太高.
// Only the first stack that is pushed, grows exponentially (2^n * STACK_PER_RECURSION) from then
// on. This flag has performance relevant characteristics. Don't set it too high.
const STACK_PER_RECURSION: usize = 1 * 1024 * 1024; // 1MB

// 添加注释: 按需增加堆栈以防止堆栈溢出. 在战略位置调用它以`break up`递归调用. 例如, 几乎任何对`visit_expr`或等
// 效的调用都可以从中受益.
/// Grows the stack on demand to prevent stack overflow. Call this in strategic locations
/// to "break up" recursive calls. E.g. almost any call to `visit_expr` or equivalent can benefit
/// from this.
///
// 添加注释: 不应该不小心洒在周围, 因为它会导致一点点开销
/// Should not be sprinkled around carelessly, as it causes a little bit of overhead.
pub fn ensure_sufficient_stack<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, STACK_PER_RECURSION, f)
}
