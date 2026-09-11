//! crate 内部的日志宏。
//!
//! 每个宏都有两份定义，由 `debug-print` feature 选择编译哪一份：启用时转发给 `tracing`，
//! 未启用时展开为空操作（参数不会被求值）。因此默认构建下不会输出任何日志，
//! 也就不会把请求体、签名串这类敏感内容写出去。

/// 记录错误日志（需启用 `debug-print`；否则编译为下方的空实现）。
#[cfg(feature = "debug-print")]
#[macro_export]
macro_rules! error {
    ($name:expr)=>{
        tracing::error!($name);
    };
   ($name:expr $(,$arg:tt)+) => {
       tracing::error!($name, $($arg),*);
    };
}

/// `error!` 的空实现：未启用 `debug-print` 时什么都不做，参数不求值。
#[cfg(not(feature = "debug-print"))]
#[macro_export]
macro_rules! error {
    ($name:expr) => {};
    ($name:expr $(,$arg:tt)+ $(,)?) => {};
}

/// 记录调试日志（需启用 `debug-print`；否则编译为下方的空实现）。
///
/// 开启后会输出请求体、Authorization 头与被签名的原文，含 openid、订单号、金额 ——
/// ⚠ 生产环境不要开启。
#[cfg(feature = "debug-print")]
#[macro_export]
macro_rules! debug {
    ($name:expr)=>{
        tracing::debug!($name);
    };
    ($name:expr $(,$arg:tt)+) => {
        tracing::debug!($name, $($arg),*);
    };
}

/// `debug!` 的空实现：未启用 `debug-print` 时什么都不做，参数不求值。
#[cfg(not(feature = "debug-print"))]
#[macro_export]
macro_rules! debug {
    ($name:expr) => {};
    ($name:expr $(,$arg:tt)+ $(,)?) => {};
}
