//! Tauri 命令层。
//!
//! 边界很明确：**执行语义全在 `apicase-core`，这里只有桌面壳专属的能力**。
//!
//! | 模块 | 归属 | 为什么不在 core |
//! |---|---|---|
//! | `exec` | 转发到 core | 只做 JSON ↔ 类型转换与事件推送 |
//! | `cookies` | 转发到 core | jar 的读 / 删 / 清，语义在 core |
//! | `fs` | 桌面壳 | 隐藏项过滤、二进制嗅探是为文件树服务的，CLI 用不上 |
//! | `watch` | 桌面壳 | 实时刷新文件树；CLI 是一次性执行，没有"树"要刷 |
//! | `terminal` | 桌面壳 | 底部终端面板 |
//! | `app` | 桌面壳 | 应用配置目录由 Tauri 按 identifier 推导 |
//!
//! 这条边界一旦破掉（比如为了省事把变量替换写进 `exec`），`apicase run` CLI
//! 就得把那段逻辑重写一遍——那正是这次改造要消灭的东西。

pub mod ai;
pub mod app;
pub mod cookies;
pub mod exec;
pub mod fs;
pub mod terminal;
pub mod watch;
