//! AirCtx 客户端库:HTTP 客户端、配置、daemon IPC 协议、同步引擎与 daemon 运行时。
//! 被 `airctx` 二进制复用;将来 npm 壳启动的也是同一个二进制。

pub mod config;
pub mod daemon;
pub mod http;
pub mod ipc;
pub mod sessions;
pub mod sources;
pub mod sync;

mod codex;
