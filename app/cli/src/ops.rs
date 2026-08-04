//! 操作层：CLI 与 MCP **共用**的一层，把「用户想做的事」翻译成对 `apicase-core` 的调用。
//!
//! # 硬约束：这一层不 print
//!
//! 它返回数据，怎么显示由外壳决定（终端渲染成表格、MCP 渲染成 JSON）。
//! 这不只是洁癖——MCP 的 stdio 传输规定 **stdout 只能有协议消息**，
//! 这里漏一行 `println!` 就会破坏 JSON-RPC 帧，表现为 MCP 客户端「连上了但什么都不工作」，
//! 且几乎无从定位。运行进度这类需要实时冒出来的东西走回调，由外壳决定是打一行字还是丢掉。
//!
//! # 为什么要有这一层
//!
//! CLI 与 MCP 若各写一遍业务，两者必然漂移——这正是执行语义下沉 core 时要消灭的那类
//! 技术债，不该在上层重新长出来。有了它，每个 CLI 子命令与每个 MCP 工具都只是同一个
//! 函数的两种外壳，参数名与语义一一对应：AI 读过 `apicase run --help` 就会用
//! `apicase_run` 工具，反之亦然。

use std::path::Path;

pub mod check;
pub mod list;
pub mod run;

pub use check::{check, check_text, CheckReport};
pub use list::list;
pub use run::{run, OnEvent, ReportSink, RunRequest};

/// 工具版本，写进报告头（`RunReport.tool.version`）。
pub fn tool_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 读一个文本文件，错误信息带上路径——「No such file or directory」不说是哪个文件时等于没说。
pub(crate) fn read_text(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("读取 {} 失败：{e}", p.display()))
}
