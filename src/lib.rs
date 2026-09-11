#![doc = include_str!("../README.md")]
// 本 crate 不含任何 unsafe 代码：所有类型依赖自动派生的 Send/Sync，
// 原先的 22 处 `unsafe impl Send/Sync` 已删除。
// 编译期由 tests/offline.rs::public_types_are_send_and_sync 守住这个性质。
#![forbid(unsafe_code)]
pub mod async_impl;
pub mod cert;
pub mod error;
pub(crate) mod macros;
pub mod model;
pub mod notify;
pub mod pay;
pub mod pay_type;
pub mod request;
pub mod response;
pub mod sign;
pub mod util;
