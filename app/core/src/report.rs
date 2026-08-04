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

/// v2：`StepStatus` 新增 `skipped`。**加字段本可以不动版本号**（旧解析器忽略即可），
/// 但新增一个状态枚举值不兼容——旧报告页遇到没见过的状态，不知道该渲染成什么颜色、
/// 该不该计入统计。版本号是为它动的。
pub const REPORT_SCHEMA_VERSION: u32 = 2;

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

    /// 截断报文体到 `max_bytes` 以内。
    ///
    /// 单文件 HTML 会把这些内容全内联，几十个大 JSON 就能把报告顶到浏览器打不开的体积。
    /// 切点落在**字符边界**上，不会切出半个 UTF-8 字符；`bytes` 记的是原始大小，
    /// 截断后仍能看出响应到底多大。
    pub fn clip(body: Option<&str>, max_bytes: usize) -> Self {
        let Some(s) = body else {
            return Self::absent();
        };
        let bytes = s.len();
        if bytes <= max_bytes {
            return Self { preview: Some(s.to_string()), bytes, truncated: false };
        }
        // 从上限位置向前退到最近的字符边界（最多退 3 个字节）
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        Self { preview: Some(s[..end].to_string()), bytes, truncated: true }
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
///
/// `skipped` 是第三类：**这一步根本没跑**——上游挂了，它依赖的输入不存在。
/// 它既不是失败也不是通过，报告里该是第三种颜色（对齐 TestNG 的 SKIP 语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Passed,
    Failed,
    Error,
    Skipped,
    Running,
}

impl StepStatus {
    /// 这一步的失败是否该挡住下游。
    ///
    /// `error` 恒阻断——请求没发出去，outputs 必然为空，下游拿到的只会是
    /// `${{steps.x.outputs.y}}` 字面量，产出 100% 是噪音，且会把脏请求打到被测服务上。
    /// `skipped` 同样阻断（传递性：没跑的节点没有产出）。
    /// 只有 `failed` 留了余地——请求成功、outputs 有真值，下游技术上跑得动。
    pub fn blocks_downstream(self, continue_on_assertion_failure: bool) -> bool {
        match self {
            StepStatus::Error | StepStatus::Skipped => true,
            StepStatus::Failed => !continue_on_assertion_failure,
            StepStatus::Passed | StepStatus::Running => false,
        }
    }
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
    pub outputs: BTreeMap<String, serde_json::Value>,
    pub assertions: Vec<AssertRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// skipped 时说明原因（形如 `上游 login 失败`）——与 `CaseResult.skip_reason`
    /// 同名同义，都回答"为什么没跑"。两级的 skipped 靠这句话区分：
    /// case 级写"读取失败：…"，step 级写"上游 X 失败"。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
}

impl StepResult {
    /// 未执行的 step 占位：**跳过的节点也要进报告**——报告里少一个节点，
    /// 看的人无从判断它是"跑过且通过"还是"压根没跑"。
    pub fn skipped(id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: StepStatus::Skipped,
            duration_ms: 0,
            request: None,
            response: None,
            outputs: BTreeMap::new(),
            assertions: Vec::new(),
            error: None,
            skip_reason: Some(reason.into()),
        }
    }
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

/// 各 step 的状态计数。**与 case 级计数分开**：用例级的 `skipped` 是「这个文件没跑」
/// （读不到 / 不是有效用例），step 级的是「上游挂了没轮到它」，两类事合并计数会让排查时分不清。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StepSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub error: u32,
    pub skipped: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RunSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub error: u32,
    pub skipped: u32,
    pub assertions: AssertSummary,
    /// 请求（step）维度的计数。旧报告没有这个键，`default` 让它们仍读得回来
    #[serde(default)]
    pub steps: StepSummary,
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
                s.steps.total += 1;
                match st.status {
                    StepStatus::Passed => s.steps.passed += 1,
                    StepStatus::Failed => s.steps.failed += 1,
                    StepStatus::Error => s.steps.error += 1,
                    StepStatus::Skipped => s.steps.skipped += 1,
                    StepStatus::Running => {}
                }
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
/// 半年后回看一份失败报告，"当时用的哪套环境、截断阈值多少"
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
    pub max_body_bytes: usize,
    /// 断言失败是否**不**阻断下游。运行面板可临时覆盖工作空间配置，故必须记进报告——
    /// 否则两份结论不同的报告摆在一起，分不清是服务变了还是选项变了。
    #[serde(default)]
    pub continue_on_assertion_failure: bool,
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
                        target: "res.status".into(),
                        op: "eq".into(),
                        expected: "200".into(),
                        actual: "200".into(),
                        ok: *ok,
                    })
                    .collect(),
                error: None,
                skip_reason: None,
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

    /// step 计数与 case 计数是**两个维度**：4 个 case 各带 1 个 passed step，
    /// 而其中 3 个 case 并非 passed —— 两行数字本就不该一致。
    #[test]
    fn summary_counts_steps_separately_from_cases() {
        let mut cases = vec![case(CaseStatus::Failed, &[false])];
        cases[0].steps.push(StepResult::skipped("s2", "上游 s1 失败"));
        cases[0].steps.push(StepResult::skipped("s3", "上游 s1 失败"));
        let s = RunSummary::of(&cases);
        assert_eq!((s.total, s.skipped), (1, 0), "用例级 skipped 只算「用例本身没跑」");
        assert_eq!(
            (s.steps.total, s.steps.passed, s.steps.failed, s.steps.error, s.steps.skipped),
            (3, 1, 0, 0, 2),
            "step 级要如实算出 2 个跳过"
        );
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
                max_body_bytes: 65536,
                continue_on_assertion_failure: false,
            },
            summary: RunSummary::default(),
            cases: vec![case(CaseStatus::Passed, &[true])],
        };
        let json = serde_json::to_string(&r).expect("序列化");
        assert!(json.contains("\"schemaVersion\":2"), "字段名必须是 camelCase：{json}");
        assert!(json.contains("\"finishedAt\":null"), "运行中 finishedAt 是 null");
        assert_eq!(serde_json::from_str::<RunReport>(&json).expect("反序列化"), r);
    }

    /// 跳过占位不带请求 / 响应 / 断言——它压根没跑
    #[test]
    fn skipped_step_carries_only_a_reason() {
        let s = StepResult::skipped("queryOrder", "上游 payOrder 失败");
        assert_eq!(s.status, StepStatus::Skipped);
        assert_eq!(s.skip_reason.as_deref(), Some("上游 payOrder 失败"));
        assert!(s.request.is_none() && s.response.is_none() && s.assertions.is_empty());
        assert_eq!(s.duration_ms, 0);
        let json = serde_json::to_string(&s).expect("序列化");
        assert!(json.contains("\"status\":\"skipped\""), "{json}");
        assert!(json.contains("\"skipReason\""), "字段名必须是 camelCase：{json}");
        // 没跳过的 step 不该带这个键
        let passed = serde_json::to_string(&case(CaseStatus::Passed, &[true]).steps[0]).unwrap();
        assert!(!passed.contains("skipReason"), "空 skipReason 不落盘：{passed}");
    }

    /// 阻断规则：error / skipped 恒阻断，failed 看开关，passed 从不阻断
    #[test]
    fn blocking_rule_depends_on_status_and_switch() {
        for (st, off, on) in [
            (StepStatus::Error, true, true),
            (StepStatus::Skipped, true, true),
            (StepStatus::Failed, true, false),
            (StepStatus::Passed, false, false),
            (StepStatus::Running, false, false),
        ] {
            assert_eq!(st.blocks_downstream(false), off, "{st:?} 关开关时");
            assert_eq!(st.blocks_downstream(true), on, "{st:?} 开开关时");
        }
    }
}
