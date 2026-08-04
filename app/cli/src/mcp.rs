//! MCP 服务器（stdio）：把 `ops` 的能力暴露给 AI Agent。
//!
//! # 这一层要薄
//!
//! 只做「工具定义 + 参数解包 + 调 ops + 把结果转成 `CallToolResult`」，**业务零行**。
//! MCP 协议正在快速演进（2025-06-18 → 2025-11-25 → 2026-07-28，最后一版是破坏性的
//! 无状态改造），协议帧的兼容矩阵交给官方 SDK；万一要换 SDK 或换传输，受影响的只有这个文件。
//!
//! 这与「YAML 输出器自己写」并不矛盾：那个是**产品面**（用户要读、要 diff 的源文件格式），
//! 这个是协议帧，产品面为零，唯一的价值是「能被各家客户端连上」。
//!
//! # stdout 纪律
//!
//! stdio 传输规定 **stdout 只能有 JSON-RPC 消息**，日志一律走 stderr。
//! `ops` 层的「不 print」约束就是为这条服务的——那里漏一行 `println!`，
//! 表现是 MCP 客户端「连上了但什么都不工作」，且几乎无从定位。
//!
//! # 与 CLI 的默认值差异
//!
//! `apicase_run` **默认不落盘 HTML 报告**（CLI 默认落）。AI 调 run 多半是
//! 「验证我刚写的这个用例对不对」，每次甩一份几 MB 的 HTML 进工作空间是污染。

use crate::{docs, ops};
use apicase_core::runner::Cancel;
use apicase_core::workspace::Workspace;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;

/// 服务器持有的是**工作空间的定位方式**而不是打开好的 `Workspace`：
/// 用例文件与 `application.yml` 在会话期间会被 AI 改写，每次调用重新读一次，
/// 才不会出现「改了配置但工具还用着旧的」。
#[derive(Clone)]
pub struct ApicaseMcp {
    root: Option<PathBuf>,
}

// ── 工具参数 ────────────────────────────────────────
//
// 字段名与 CLI 的选项一一对应（`target` ↔ 位置参数、`env` ↔ `-e`、`steps` ↔ `--step`…）：
// 三处同名同义，AI 读过 `apicase run --help` 就会用这个工具，反之亦然。

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunParams {
    /// 用例文件或目录（相对工作空间根或绝对路径）。省略 = 整个工作空间
    #[serde(default)]
    pub target: Option<String>,
    /// 直接给一段 case YAML 跑，不落盘。给了就忽略 target
    #[serde(default)]
    pub content: Option<String>,
    /// 环境名。省略 = 工作空间的缺省环境
    #[serde(default)]
    pub env: Option<String>,
    /// 只跑这些请求（自动带上它们的上游依赖）
    #[serde(default)]
    pub steps: Vec<String>,
    /// 追加或覆盖环境变量
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
    /// 用例之间的并发数，默认 1
    #[serde(default)]
    pub concurrency: Option<u32>,
    /// 首个失败即停
    #[serde(default)]
    pub bail: bool,
    /// 详略：summary（默认，统计 + 失败现场）| full（完整报告，可能很大）
    #[serde(default)]
    pub detail: Option<String>,
    /// 落一份 HTML 报告到 .apicase/reports/（默认不落）
    #[serde(default)]
    pub report: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CheckParams {
    /// 用例文件或目录。省略 = 整个工作空间
    #[serde(default)]
    pub target: Option<String>,
    /// 直接校验一段 case YAML（写完先自检，比跑一轮快得多）
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListParams {
    /// 目录或文件。省略 = 整个工作空间
    #[serde(default)]
    pub path: Option<String>,
    /// 按文件路径 / 用例名 / 请求 URL 过滤（子串，大小写不敏感）
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShowParams {
    /// 用例文件
    pub target: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EnvParams {
    /// 环境名。省略 = 列出所有环境
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReportParams {
    /// 报告文件名或路径。省略 = 最近一份
    #[serde(default)]
    pub file: Option<String>,
    /// 只看这些用例：all（默认）| failed | error | bad（失败与错误）
    #[serde(default)]
    pub filter: Option<String>,
    /// 详略：summary（默认）| full
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DocsParams {
    /// 主题。省略 = case（用例的整体结构）
    #[serde(default)]
    pub topic: Option<String>,
}

// ── 工具实现 ────────────────────────────────────────

#[tool_router]
impl ApicaseMcp {
    /// 不存 `ToolRouter`：`#[tool_handler]` 走的是 `Self::tool_router()`（宏生成的关联函数），
    /// 存一份字段只是让每个实例多背一张表。
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }

    /// 运行用例并返回结果。
    ///
    /// 返回一份运行报告：统计（用例 / 请求 / 断言三个维度）+ 每个用例的状态。
    /// 默认只带失败现场（失败的断言、错误原因、失败请求的响应），
    /// 通过的部分不占上下文；要完整报文传 detail="full"。
    #[tool(name = "apicase_run")]
    pub async fn run(&self, Parameters(p): Parameters<RunParams>) -> Result<CallToolResult, McpError> {
        let hint = p.target.as_deref().map(PathBuf::from);
        let ws = self.workspace(hint.as_deref())?;

        let req = ops::RunRequest {
            targets: p.target.iter().map(|t| resolve(&ws, t)).collect(),
            content: p.content,
            env: p.env,
            steps: p.steps,
            vars: p.vars.into_iter().collect(),
            concurrency: p.concurrency.unwrap_or(1).max(1),
            stop_on_failure: p.bail,
            recursive: true,
            // AI 高频调用，默认不往工作空间里甩几 MB 的 HTML
            report: if p.report { ops::ReportSink::Auto } else { ops::ReportSink::None },
            proxy: crate::resolve_proxy(),
            ..Default::default()
        };

        let outcome = ops::run(&ws, &req, None, Cancel::new()).await.map_err(bad_request)?;
        let full = p.detail.as_deref() == Some("full");
        let report = crate::shrink(&outcome.report, if full { crate::Detail::Full } else { crate::Detail::Summary });
        let mut v = serde_json::to_value(&report).map_err(internal)?;
        if let (Some(o), Some(path)) = (v.as_object_mut(), outcome.report_path.as_ref()) {
            o.insert("reportFile".into(), json!(path.to_string_lossy()));
        }
        Ok(CallToolResult::structured(v))
    }

    /// 校验用例，只解析不发请求。
    ///
    /// 比跑一轮快得多，且能查出「跑得动但注定没意义」的问题：依赖指向不存在的请求、
    /// 断言目标写成认不出的形式、请求 id 重复、依赖成环。**写完用例先过这一道。**
    #[tool(name = "apicase_check")]
    pub async fn check(&self, Parameters(p): Parameters<CheckParams>) -> Result<CallToolResult, McpError> {
        if let Some(text) = p.content {
            let r = ops::CheckReport::of_one(ops::check_text(&text, "<inline>"));
            return Ok(CallToolResult::structured(serde_json::to_value(r).map_err(internal)?));
        }
        let hint = p.target.as_deref().map(PathBuf::from);
        let ws = self.workspace(hint.as_deref())?;
        let targets: Vec<PathBuf> = p.target.iter().map(|t| resolve(&ws, t)).collect();
        let r = ops::check(&ws, &targets, true);
        Ok(CallToolResult::structured(serde_json::to_value(r).map_err(internal)?))
    }

    /// 列出工作空间里的用例：路径、名称、每个请求的方法与 URL、依赖关系。
    ///
    /// 解析失败的用例也会列出（带上原因），不会悄悄消失。
    #[tool(name = "apicase_list")]
    pub async fn list(&self, Parameters(p): Parameters<ListParams>) -> Result<CallToolResult, McpError> {
        let hint = p.path.as_deref().map(PathBuf::from);
        let ws = self.workspace(hint.as_deref())?;
        let targets: Vec<PathBuf> = p.path.iter().map(|t| resolve(&ws, t)).collect();
        let items = ops::list(&ws, &targets, true, p.query.as_deref());
        Ok(CallToolResult::structured(json!({ "cases": items, "total": items.len() })))
    }

    /// 读一个用例：结构化模型 + 规范化后的 YAML 原文。
    #[tool(name = "apicase_show")]
    pub async fn show(&self, Parameters(p): Parameters<ShowParams>) -> Result<CallToolResult, McpError> {
        let ws = self.workspace(Some(&PathBuf::from(&p.target)))?;
        let path = resolve(&ws, &p.target);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| bad_request(format!("读取 {} 失败：{e}", path.display())))?;
        let a = apicase_core::yaml::analyze_case(&text);
        match a.case.filter(|_| a.valid) {
            Some(c) => Ok(CallToolResult::structured(json!({
                "file": ws.rel(&path),
                "case": c,
                "yaml": apicase_core::yaml::dump_case(&c),
            }))),
            // 解析不了不是协议错误，是「这个用例有问题」——要让 AI 读到原因并去改它
            None => Ok(CallToolResult::structured(json!({
                "file": ws.rel(&path),
                "valid": false,
                "error": a.error.unwrap_or_else(|| "不是有效的用例".into()),
                "raw": text,
            }))),
        }
    }

    /// 列出环境，或看某一套环境的变量。
    #[tool(name = "apicase_env")]
    pub async fn env(&self, Parameters(p): Parameters<EnvParams>) -> Result<CallToolResult, McpError> {
        let ws = self.workspace(None)?;
        let v = match p.name {
            Some(n) => serde_json::to_value(ws.env_info(Some(&n))).map_err(internal)?,
            None => json!({ "environments": ws.env_names(), "default": ws.default_env() }),
        };
        Ok(CallToolResult::structured(v))
    }

    /// 读一份运行报告（默认最近一份）。看的是历史结果，不重新跑。
    #[tool(name = "apicase_report")]
    pub async fn report(&self, Parameters(p): Parameters<ReportParams>) -> Result<CallToolResult, McpError> {
        let ws = self.workspace(None)?;
        let path = crate::pick_report(&ws, p.file.map(PathBuf::from)).map_err(bad_request)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| bad_request(format!("读取 {} 失败：{e}", path.display())))?;
        let mut report = apicase_core::render::parse_report_html(&text)
            .ok_or_else(|| bad_request(format!("{} 不是 apicase 生成的报告", path.display())))?;

        use apicase_core::report::CaseStatus;
        match p.filter.as_deref() {
            Some("failed") => report.cases.retain(|c| c.status == CaseStatus::Failed),
            Some("error") => report.cases.retain(|c| c.status == CaseStatus::Error),
            Some("bad") => report
                .cases
                .retain(|c| !matches!(c.status, CaseStatus::Passed | CaseStatus::Running)),
            _ => {}
        }
        let full = p.detail.as_deref() == Some("full");
        let out = crate::shrink(&report, if full { crate::Detail::Full } else { crate::Detail::Summary });
        let mut v = serde_json::to_value(&out).map_err(internal)?;
        if let Some(o) = v.as_object_mut() {
            o.insert("reportFile".into(), json!(path.to_string_lossy()));
        }
        Ok(CallToolResult::structured(v))
    }

    /// apicase 用例的 YAML 格式规范。**写用例之前先读这个。**
    ///
    /// 主题：case（默认，整体结构）/ assertions / auth / body / vars / settings / all。
    #[tool(name = "apicase_docs")]
    pub async fn docs(&self, Parameters(p): Parameters<DocsParams>) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(json!({
            "topic": p.topic.clone().unwrap_or_else(|| "case".into()),
            "availableTopics": docs::TOPICS.iter().map(|t| json!({"name": t.name, "about": t.about})).collect::<Vec<_>>(),
            "markdown": docs::topic(p.topic.as_deref()),
        })))
    }

    /// 每次调用重新打开工作空间：会话期间 AI 会改 `application.yml` 与用例，
    /// 缓存住只会让工具用着过时的配置。开销是读一个几 KB 的文件。
    fn workspace(&self, hint: Option<&std::path::Path>) -> Result<Workspace, McpError> {
        if let Some(root) = &self.root {
            return Workspace::open(root).map_err(bad_request);
        }
        let start = hint
            .filter(|p| p.is_absolute())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(crate::cwd);
        Workspace::discover(&start).map_err(bad_request)
    }
}

#[tool_handler]
impl ServerHandler for ApicaseMcp {
    fn get_info(&self) -> ServerInfo {
        // 逐字段赋值而不是结构体字面量：rmcp 的这两个类型是 `#[non_exhaustive]`，
        // 字面量写法连带 `..Default::default()` 都不允许（那正是它想防的——
        // 上游加字段时不该让下游编译不过）
        let mut who = Implementation::from_build_env();
        who.name = "apicase".into();
        who.title = Some("apicase".into());
        who.version = ops::tool_version();
        who.description = Some("API 用例的运行、校验与查看".into());

        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = who;
        // instructions 是**开场白**，client 会把它放进系统提示。
        // 这里写的是「怎么用这套工具」的最小闭环，AI 因此不必先试错一轮。
        info.instructions = Some(
                "apicase 把 API 用例写成 YAML 文件（一个 .yml = 一个用例，内部是请求的 DAG），\
                 用它们做接口调试与回归。\n\n\
                 用例文件用你自己的文件读写工具直接编辑即可，本服务不提供写入工具。\
                 典型闭环：\n\
                 1. apicase_docs 查格式（写用例之前先读，schema 靠猜必然写错）\n\
                 2. 用文件工具写 .yml\n\
                 3. apicase_check 自检（不发请求，秒回）\n\
                 4. apicase_run 验证\n\
                 5. 失败就看返回里的断言现场（target / expected / actual）再改\n\n\
                 结果里 failed 与 error 是两回事：failed 是请求发出去了但断言没过（被测服务的问题），\
                 error 是请求本身失败（网络 / TLS / 超时，多半是环境或用例自身的问题）。\
                 skipped 是上游挂了没轮到它跑。"
                .into(),
        );
        info
    }
}

/// 起服务器，直到 stdin 关闭。
///
/// 日志走 stderr：stdout 被协议占着，往那儿写一个字节就会破坏 JSON-RPC 帧。
pub async fn serve(root: Option<PathBuf>) -> Result<(), String> {
    // 工作空间在这里先验一次：连上之后每个工具调用都报同一个错，不如启动时就说清楚
    if let Some(r) = &root {
        Workspace::open(r)?;
    }
    eprintln!(
        "apicase MCP 服务器已启动（stdio）· 工作空间 {}",
        root.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "按调用方的工作目录推导".into())
    );

    let service = ApicaseMcp::new(root)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| format!("MCP 服务器启动失败：{e}"))?;
    service.waiting().await.map_err(|e| format!("MCP 服务器异常退出：{e}"))?;
    Ok(())
}

/// 相对路径按**工作空间根**解析，而不是按进程的工作目录。
///
/// MCP 客户端的 cwd 是它自己的（常是编辑器的启动目录），跟工作空间没关系。
/// AI 手里的路径来自 `apicase_list` 的输出——那是相对工作空间根的。
fn resolve(ws: &Workspace, target: &str) -> PathBuf {
    let p = PathBuf::from(target);
    if p.is_absolute() {
        p
    } else {
        ws.root.join(p)
    }
}

/// 参数有问题、文件读不到、工作空间找不到 —— 这些是**调用方能修的**，
/// 用 `INVALID_PARAMS` 让消息原样传到 AI 面前。
fn bad_request(msg: impl std::fmt::Display) -> McpError {
    McpError::new(rmcp::model::ErrorCode::INVALID_PARAMS, msg.to_string(), None)
}

/// 序列化这类「服务器自己坏了」的错误。MCP 客户端多半只显示一句通用文案，
/// 所以真正要给 AI 看的信息不该走这条路。
fn internal(e: impl std::fmt::Display) -> McpError {
    McpError::new(rmcp::model::ErrorCode::INTERNAL_ERROR, e.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 工具清单是**对外契约**：改名等于让已经配好的 AI Agent 突然找不到工具。
    #[test]
    fn tool_names_and_schemas_are_stable() {
        let router = ApicaseMcp::tool_router();
        let mut names: Vec<String> = router.list_all().into_iter().map(|t| t.name.to_string()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "apicase_check",
                "apicase_docs",
                "apicase_env",
                "apicase_list",
                "apicase_report",
                "apicase_run",
                "apicase_show",
            ],
            "工具名是对外契约，改名要同步文档与已配好的 Agent"
        );
    }

    /// 每个工具都要有描述——AI 靠它决定调不调这个工具，空描述等于这个工具不存在
    #[test]
    fn every_tool_is_described() {
        for t in ApicaseMcp::tool_router().list_all() {
            let d = t.description.as_deref().unwrap_or("");
            assert!(d.len() > 10, "工具 {} 的描述太短：{d:?}", t.name);
        }
    }

    /// 开场白要把 failed / error 的区别说清楚——这是 apicase 结果里最容易被误读的一处
    #[test]
    fn instructions_explain_the_workflow_and_the_status_semantics() {
        let info = ApicaseMcp::new(None).get_info();
        let i = info.instructions.expect("应有开场白");
        for must in ["apicase_docs", "apicase_check", "apicase_run", "failed", "error", "skipped"] {
            assert!(i.contains(must), "开场白里应提到 `{must}`");
        }
        assert_eq!(info.server_info.name, "apicase");
    }

    /// 相对路径按工作空间根解析：MCP 客户端的 cwd 与工作空间没有关系
    #[test]
    fn relative_targets_resolve_against_the_workspace_root() {
        let dir = std::env::temp_dir().join("apicase-mcp-resolve");
        std::fs::create_dir_all(&dir).expect("建目录");
        let ws = Workspace::open(&dir).expect("应能打开");
        assert_eq!(resolve(&ws, "api/login.yml"), dir.join("api/login.yml"));
        assert_eq!(resolve(&ws, "/abs/login.yml"), PathBuf::from("/abs/login.yml"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
