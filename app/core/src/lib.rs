//! apicase 执行内核。
//!
//! 一句话职责：**从 case 文件到运行报告的全部语义，都在这里**——
//! YAML 解析、变量透传、请求组装、认证、发送、输出提取、断言、脱敏、报告渲染。
//!
//! 上层（Tauri 桌面壳、将来的 `apicase run` CLI）只做三件事：
//! 给输入、注入 IO、把产物交给用户。它们之间不共享任何执行语义，
//! 因此不存在「界面里跑过了、CLI 跑却挂了」这类两套实现必然产生的漂移。
//!
//! ```text
//!            ┌─────────────── apicase-core ───────────────┐
//!  case.yml →│ yaml → vars → request → auth → http        │→ RunReport → report.html
//!            │              ↓                              │
//!            │        jsonpath → assert → redact           │
//!            └────────────────────────────────────────────┘
//!                    ↑                        ↑
//!            Tauri 命令层               CLI（后续）
//! ```

pub mod assert;
pub mod auth;
pub mod http;
pub mod jsonpath;
pub mod model;
pub mod redact;
pub mod render;
pub mod report;
pub mod request;
pub mod runner;
pub mod util;
pub mod vars;
pub mod yaml;

#[cfg(test)]
mod testutil;

pub use model::*;
pub use report::*;
