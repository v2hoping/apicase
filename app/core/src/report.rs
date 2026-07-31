//! 运行报告的数据模型 —— 执行与渲染之间的**契约**。
//!
//! 三层分工（对齐 Allure / Playwright / Newman）：
//!
//! ```text
//!   runner  →  RunReport（纯数据）  →  render::html / 将来的 junit / json
//! ```
//!
//! 执行只产出结构化数据，渲染是纯函数。加一种输出格式 = 加一个 renderer，
//! 执行引擎一行不改；而 `apicase run` 接 CI 时，`RunReport` 就是现成的契约。
//!
//! `schemaVersion` 是给未来的自己留的门：报告文件会被归档、被历史回看，
//! 字段语义变了必须能识别出来，而不是让老报告静默地渲染错。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvPair {
    pub key: String,
    pub value: String,
}

impl KvPair {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self { key: key.into(), value: value.into() }
    }
}

/// 一段被记录进报告的报文体：超限即截断，但 `bytes` 记的始终是**原始**大小——
/// 截断后仍能看出响应到底多大，否则"是不是接口返回爆了"就成了盲区。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyRecord {
    /// None = 根本没有报文体（区别于 `Some("")` 的空体）
    pub preview: Option<String>,
    pub bytes: usize,
    pub truncated: bool,
}

impl BodyRecord {
    pub fn absent() -> Self {
        Self { preview: None, bytes: 0, truncated: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssertRecord {
    pub target: String,
    pub op: String,
    /// `exists` / `notExists` 无期望值，用 `—` 占位（与响应区断言栏一致）
    pub expected: String,
    pub actual: String,
    pub ok: bool,
}

/// **`failed` 与 `error` 必须分辨**：前者是请求发出去了但断言没过（被测服务的问题），
/// 后者是请求本身失败（网络 / TLS / 超时 / 变量解析崩了，常是环境或用例自身的问题）。
/// 两者的排查方向完全不同，混成一个状态等于丢掉最有用的那点信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Passed,
    Failed,
    Error,
    Running,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRecord {
    pub method: String,
    pub url: String,
    pub headers: Vec<KvPair>,
    pub body: BodyRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseRecord {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<KvPair>,
    pub body: BodyRecord,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub id: String,
    pub status: StepStatus,
    pub duration_ms: u64,
    /// None = 请求还没组装出来就失败了（变量解析 / 认证前置步骤）
    pub request: Option<RequestRecord>,
    pub response: Option<ResponseRecord>,
    /// 报告里存的是**脱敏版**；透传给下游 step 的是原值（见 runner）
    pub outputs: BTreeMap<String, serde_json::Value>,
    pub assertions: Vec<AssertRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaseStatus {
    Passed,
    Failed,
    Error,
    Skipped,
    Running,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseResult {
    /// 相对工作空间根的路径（报告要能跨机器看，绝对路径没有意义）
    pub file: String,
    /// case 的 `name`，缺省用文件名
    pub name: String,
    pub status: CaseStatus,
    /// skipped 时说明原因——**静默跳过会让人误以为全跑过了**
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub started_at: String,
    pub duration_ms: u64,
    pub steps: Vec<StepResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AssertSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RunSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub error: u32,
    pub skipped: u32,
    pub assertions: AssertSummary,
}

impl RunSummary {
    pub fn of(cases: &[CaseResult]) -> Self {
        let mut s = Self { total: cases.len() as u32, ..Default::default() };
        for c in cases {
            match c.status {
                CaseStatus::Passed => s.passed += 1,
                CaseStatus::Failed => s.failed += 1,
                CaseStatus::Error => s.error += 1,
                CaseStatus::Skipped => s.skipped += 1,
                CaseStatus::Running => {}
            }
            for st in &c.steps {
                for a in &st.assertions {
                    s.assertions.total += 1;
                    if a.ok {
                        s.assertions.passed += 1;
                    } else {
                        s.assertions.failed += 1;
                    }
                }
            }
        }
        s
    }
}

/// 报告头部记录的运行参数——**报告要能自证是怎么跑出来的**。
/// 半年后回看一份失败报告，"当时用的哪套环境、脱没脱敏、截断阈值多少"
/// 直接决定了结论能不能信。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOptions {
    /// 用户选中的目标（目录或文件，相对工作空间根）
    pub targets: Vec<String>,
    pub recursive: bool,
    pub environment: String,
    pub concurrency: u32,
    pub stop_on_failure: bool,
    pub redact: bool,
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentInfo {
    pub name: String,
    /// 已脱敏
    pub vars: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub schema_version: u32,
    pub tool: ToolInfo,
    pub started_at: String,
    /// 运行中为 null
    pub finished_at: Option<String>,
    pub duration_ms: u64,
    pub status: RunStatus,
    pub workspace: WorkspaceInfo,
    pub environment: EnvironmentInfo,
    pub options: RunOptions,
    pub summary: RunSummary,
    pub cases: Vec<CaseResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(status: CaseStatus, asserts: &[bool]) -> CaseResult {
        CaseResult {
            file: "a.yml".into(),
            name: "a".into(),
            status,
            skip_reason: None,
            started_at: "2026-07-30T00:00:00.000Z".into(),
            duration_ms: 1,
            steps: vec![StepResult {
                id: "s1".into(),
                status: StepStatus::Passed,
                duration_ms: 1,
                request: None,
                response: None,
                outputs: BTreeMap::new(),
                assertions: asserts
                    .iter()
                    .map(|ok| AssertRecord {
                        target: "status".into(),
                        op: "eq".into(),
                        expected: "200".into(),
                        actual: "200".into(),
                        ok: *ok,
                    })
                    .collect(),
                error: None,
            }],
        }
    }

    #[test]
    fn summary_counts_cases_and_assertions() {
        let cases = vec![
            case(CaseStatus::Passed, &[true, true]),
            case(CaseStatus::Failed, &[true, false]),
            case(CaseStatus::Error, &[]),
            case(CaseStatus::Skipped, &[]),
        ];
        let s = RunSummary::of(&cases);
        assert_eq!((s.total, s.passed, s.failed, s.error, s.skipped), (4, 1, 1, 1, 1));
        assert_eq!((s.assertions.total, s.assertions.passed, s.assertions.failed), (4, 3, 1));
    }

    /// 状态字符串是报告 HTML 里的 CSS class 与筛选值，改了就静默错位
    #[test]
    fn status_serializes_to_stable_strings() {
        assert_eq!(serde_json::to_string(&StepStatus::Passed).unwrap(), "\"passed\"");
        assert_eq!(serde_json::to_string(&CaseStatus::Skipped).unwrap(), "\"skipped\"");
        assert_eq!(serde_json::to_string(&RunStatus::Cancelled).unwrap(), "\"cancelled\"");
    }

    /// 报告 JSON 要能被自己读回来（历史报告回看这条链路的地基）
    #[test]
    fn report_json_roundtrips() {
        let r = RunReport {
            schema_version: REPORT_SCHEMA_VERSION,
            tool: ToolInfo { name: "apicase".into(), version: "0.1.0".into() },
            started_at: "2026-07-30T00:00:00.000Z".into(),
            finished_at: None,
            duration_ms: 12,
            status: RunStatus::Running,
            workspace: WorkspaceInfo { name: "w".into(), root: "/w".into() },
            environment: EnvironmentInfo { name: "dev".into(), vars: BTreeMap::new() },
            options: RunOptions {
                targets: vec!["a".into()],
                recursive: true,
                environment: "dev".into(),
                concurrency: 1,
                stop_on_failure: false,
                redact: true,
                max_body_bytes: 65536,
            },
            summary: RunSummary::default(),
            cases: vec![case(CaseStatus::Passed, &[true])],
        };
        let json = serde_json::to_string(&r).expect("序列化");
        assert!(json.contains("\"schemaVersion\":1"), "字段名必须是 camelCase：{json}");
        assert!(json.contains("\"finishedAt\":null"), "运行中 finishedAt 是 null");
        assert_eq!(serde_json::from_str::<RunReport>(&json).expect("反序列化"), r);
    }
}
