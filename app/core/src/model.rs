//! case 的内存模型 —— 与 `docs/0.latest/3.YAML格式规范.md` 一一对应。
//!
//! 这些类型同时是三处的契约：YAML 文件的形状、IPC 给前端的 JSON 形状、执行引擎的输入。
//! 因此字段名对前端一律 camelCase（`#[serde(rename_all = "camelCase")]`），
//! 与前端 `case.ts` 里的 TS interface 逐字段对齐。
//!
//! **枚举一律容错**：`AuthType` / `BodyType` / `AssertOp` 用 `from = "String"` 反序列化，
//! 遇到不认识的取值回落到安全默认而不是报错。配置文件是手写的，一个拼错的
//! `op: equals` 不该让整份 case 变成"解析失败"——那样用户连哪一行错了都看不到。

use serde::{Deserialize, Serialize};

/// case 文件的 schema 版本（`apicase:` 字段）。
///
/// 带 `v` 前缀是刻意的：`0.1` 是数字形态，作为字符串落盘就得加引号（`apicase: '0.1'`），
/// 而且真到 `0.10` 时裸写还会被读成 `0.1`。`v0.1` 本就不是数字，裸写即可。
pub const CASE_VERSION: &str = "v0.1";

// ── 键值行 ──────────────────────────────────────────

/// 一行键值（query / headers / 表单项通用）。
/// `enabled` 默认 true；`description` 为可选备注（空串视同没有）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kv {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn yes() -> bool {
    true
}

impl Kv {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self { name: name.into(), value: value.into(), enabled: true, description: None }
    }
    /// 参与发送吗——启用且有名字。空名行是编辑器里的"下一行占位"，不该发出去。
    pub fn active(&self) -> bool {
        self.enabled && !self.name.trim().is_empty()
    }
}

/// form-data 的一项：文本字段或文件字段。
/// 文件路径直接存进 `value`（不另立子键），行结构与 `Kv` 一致——
/// 禁用 / 备注 / `{{变量}}` 替换因此全部复用，YAML 里 `type: file` 一眼可辨。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormItem {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 缺省即文本字段；只有 `file` 会被落盘与序列化（text 是默认值，不写进 YAML）
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<FormKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormKind {
    Text,
    File,
}

impl FormItem {
    pub fn is_file(&self) -> bool {
        self.kind == Some(FormKind::File)
    }
    pub fn active(&self) -> bool {
        self.enabled && !self.name.trim().is_empty()
    }
}

// ── 请求体 ──────────────────────────────────────────

/// 请求体类型；平铺到一级（借 Apifox），不做 Postman 那层 raw + 语言二级下拉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyType {
    #[default]
    None,
    Json,
    Xml,
    Text,
    FormUrlencoded,
    FormData,
    Binary,
}

impl BodyType {
    pub fn as_str(self) -> &'static str {
        match self {
            BodyType::None => "none",
            BodyType::Json => "json",
            BodyType::Xml => "xml",
            BodyType::Text => "text",
            BodyType::FormUrlencoded => "form-urlencoded",
            BodyType::FormData => "form-data",
            BodyType::Binary => "binary",
        }
    }
}

impl From<&str> for BodyType {
    fn from(s: &str) -> Self {
        match s {
            "json" => BodyType::Json,
            "xml" => BodyType::Xml,
            "text" => BodyType::Text,
            "form-urlencoded" => BodyType::FormUrlencoded,
            "form-data" => BodyType::FormData,
            "binary" => BodyType::Binary,
            _ => BodyType::None,
        }
    }
}

/// 请求体规格。各字段按 `type` 取用，其余为 None——
/// 保留而非清空是刻意的：用户在 json / form-data 之间来回切时，
/// 已填的内容不会因为切一下类型就没了。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BodySpec {
    #[serde(rename = "type")]
    pub kind: BodyType,
    /// type=json：结构化对象（diff 友好，不是字符串）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xml: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// type=text | binary 可选覆盖 Content-Type
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urlencoded: Option<Vec<Kv>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_data: Option<Vec<FormItem>>,
    /// type=binary：以原始字节发送的文件路径（由执行内核读盘）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

// ── 认证 ────────────────────────────────────────────

/// 认证方式；命名对齐 Postman / Insomnia / Bruno 的通行叫法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthType {
    #[default]
    None,
    Bearer,
    Basic,
    Apikey,
    Digest,
    Oauth2,
}

impl AuthType {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthType::None => "none",
            AuthType::Bearer => "bearer",
            AuthType::Basic => "basic",
            AuthType::Apikey => "apikey",
            AuthType::Digest => "digest",
            AuthType::Oauth2 => "oauth2",
        }
    }
}

impl From<&str> for AuthType {
    fn from(s: &str) -> Self {
        match s {
            "bearer" => AuthType::Bearer,
            "basic" => AuthType::Basic,
            "apikey" => AuthType::Apikey,
            "digest" => AuthType::Digest,
            "oauth2" => AuthType::Oauth2,
            _ => AuthType::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BearerAuth {
    #[serde(default)]
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BasicAuth {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// API Key 放请求头还是 URL 查询参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApikeyIn {
    #[default]
    Header,
    Query,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApikeyAuth {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub r#in: ApikeyIn,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DigestAuth {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// OAuth2 客户端凭据放 Basic 头还是表单体。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientAuth {
    #[default]
    Header,
    Body,
}

/// 仅客户端凭据模式（client_credentials）——自动化调试最常用的一支。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Oauth2Auth {
    #[serde(default)]
    pub token_url: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default)]
    pub client_auth: ClientAuth,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSpec {
    #[serde(rename = "type")]
    pub kind: AuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer: Option<BearerAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basic: Option<BasicAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apikey: Option<ApikeyAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<DigestAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth2: Option<Oauth2Auth>,
}

// ── HTTP 报文 ───────────────────────────────────────

/// HTTP 请求报文规格（单 / 多请求复用）。
/// 未来多协议可另立 `GrpcSpec` 等，并列于 `Step` 之下。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpSpec {
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub query: Vec<Kv>,
    #[serde(default)]
    pub headers: Vec<Kv>,
    #[serde(default)]
    pub auth: AuthSpec,
    #[serde(default)]
    pub body: BodySpec,
}

impl Default for HttpSpec {
    fn default() -> Self {
        Self {
            method: "GET".into(),
            url: String::new(),
            query: Vec::new(),
            headers: Vec::new(),
            auth: AuthSpec::default(),
            body: BodySpec::default(),
        }
    }
}

// ── 输出提取与断言 ──────────────────────────────────

/// 输出提取：`outputs: { token: $.data.token }` → `{ name: "token", path: "$.data.token" }`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepOutput {
    pub name: String,
    pub path: String,
}

/// 断言操作符（借 Step CI check / Bruno assert 的收敛形）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssertOp {
    #[default]
    Eq,
    Ne,
    Contains,
    Exists,
    NotExists,
    Gt,
    Lt,
    Matches,
}

impl AssertOp {
    pub fn as_str(self) -> &'static str {
        match self {
            AssertOp::Eq => "eq",
            AssertOp::Ne => "ne",
            AssertOp::Contains => "contains",
            AssertOp::Exists => "exists",
            AssertOp::NotExists => "notExists",
            AssertOp::Gt => "gt",
            AssertOp::Lt => "lt",
            AssertOp::Matches => "matches",
        }
    }
    /// `exists` / `notExists` 没有期望值——报告与断言栏都用 `—` 占位。
    pub fn needs_value(self) -> bool {
        !matches!(self, AssertOp::Exists | AssertOp::NotExists)
    }
}

impl From<&str> for AssertOp {
    fn from(s: &str) -> Self {
        match s {
            "ne" => AssertOp::Ne,
            "contains" => AssertOp::Contains,
            "exists" => AssertOp::Exists,
            "notExists" => AssertOp::NotExists,
            "gt" => AssertOp::Gt,
            "lt" => AssertOp::Lt,
            "matches" => AssertOp::Matches,
            // 认不出的操作符回落 eq —— 而不是让整份 case 解析失败
            _ => AssertOp::Eq,
        }
    }
}

/// 单条断言：`target` 统一挂在 `res` 下 —— `res.status` | `res.headers.<名>` | `res.body<路径>`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assertion {
    pub target: String,
    pub op: AssertOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

// ── step 与 case ────────────────────────────────────

/// 一个 step（可编排的调用节点；借 Arazzo step / GHA job）。
/// 协议由 `protocol` 显式声明（当前仅 `http`），报文承载于 `request`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    #[serde(default = "http_proto")]
    pub protocol: String,
    /// 画布坐标等前端属性；缺省时按 `dependsOn` 自动布局
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<StepUi>,
    /// 报文（YAML 键为 `request:`；内部沿用 http 命名承载 HttpSpec）
    #[serde(default)]
    pub http: HttpSpec,
    /// DAG 依赖指针（借 Arazzo dependsOn / GHA needs）
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<StepOutput>,
    #[serde(default)]
    pub assertions: Vec<Assertion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
}

fn http_proto() -> String {
    "http".into()
}

/// step 的前端属性（当前只有画布坐标）——**与执行语义无关**，跟着所属的 step 走。
///
/// 早先坐标挂在顶层 `ui.nodes.<stepId>` 上，是一张与 `steps:` 并行的 id 映射表：
/// 改个 id 要动两处、删个 step 会在那边留下孤儿坐标，diff 里也看不出坐标属于谁。
/// 挂进 step 之后这三件事自然消失，代价只是坐标混在语义字段中间——
/// 用一个 `ui:` 子键兜住就行，将来的前端属性（折叠、配色…）也往这里加。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StepUi {
    pub x: f64,
    pub y: f64,
}

/// 一个 case：统一为 step 列表（单请求 = 长度 1，多请求 = DAG）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Case {
    /// `apicase: v0.1`
    ///
    /// 带 `v` 前缀是刻意的：`0.1` 是数字形态，作为字符串落盘就得加引号
    /// （`apicase: '0.1'`），而且真到 `0.10` 时裸写还会被读成 `0.1`。
    /// `v0.1` 本就不是数字，裸写即可。
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// case 级变量；值可以是任意 YAML 标量 / 结构（发送时按需转字符串）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars: Option<serde_json::Map<String, serde_json::Value>>,
    /// 对应 YAML `steps:`（内部沿用 requests 命名，与前端 TS 一致）
    #[serde(default)]
    pub requests: Vec<Step>,
}

impl Default for Case {
    fn default() -> Self {
        Self { version: CASE_VERSION.into(), name: None, vars: None, requests: Vec::new() }
    }
}

// ── 工作空间设置 ────────────────────────────────────

/// 并行度上限。
///
/// 手滑把 `4` 写成 `400` 的代价不是「慢一点」，而是几百条连接同时打到被测服务上——
/// 那是一次压测，不是一次回归。故解析期就 clamp 住，而不是照单全收。
pub const MAX_CONCURRENCY: u32 = 64;

/// 工作空间级请求设置（`application.yml` 的 `settings:` 键）。跟随项目走 git，团队共享。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSettings {
    /// SSL/TLS 证书验证；关闭后接受任何服务端证书（降安全，UI 警示）
    pub verify_ssl: bool,
    pub use_custom_ca: bool,
    /// CA 证书文件，**相对工作空间根**的路径（绝对路径换机器就失效）
    pub ca_cert: String,
    /// 整个请求的超时上限（毫秒），0 = 不限制
    pub timeout: u64,
    /// 自动收发 Cookie（jar 存 `<workspace>/.apicase/cookies.yml`）。默认 `true`——
    /// 对齐 Postman / Bruno 与浏览器直觉：登录一次，后面的请求自然带着会话。
    pub cookies: bool,
    /// 断言失败是否**不**阻断下游 step。默认 `false` = 阻断。
    /// `error`（请求没发出去）不受此影响，恒阻断——那种情况下游拿到的只会是
    /// 未解析的 `${{...}}` 字面量，跑下去既是噪音又会把脏请求打到被测服务上。
    pub continue_on_assertion_failure: bool,
    /// **case 之间**的并发数；1 = 串行（默认）。case 内部的 step 恒按拓扑序串行——
    /// 它们之间有 `dependsOn` 与 outputs 传递，并发跑没有意义。
    ///
    /// 归工作空间而非应用设置：「这套接口能扛多少并发回归」是被测服务的属性，
    /// 换台电脑不会变，且团队该按同一个值跑，否则「我这儿过了你那儿挂」查不出根因。
    /// 取值恒在 `1..=MAX_CONCURRENCY`（`parse_settings` 已 clamp）。
    pub concurrency: u32,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            verify_ssl: true,
            use_custom_ca: false,
            ca_cert: String::new(),
            timeout: 0,
            cookies: true,
            continue_on_assertion_failure: false,
            concurrency: 1,
        }
    }
}

// ── 枚举的 serde 桥接 ───────────────────────────────
//
// 三个 `as_str` / `From<&str>` 枚举统一走「字符串中转」的序列化：
// 反序列化时任何认不出的取值都回落到 Default，而不是让 serde 报 unknown variant。

macro_rules! str_enum_serde {
    ($t:ty) => {
        impl Serialize for $t {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }
        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                // 不用 &str：JSON 里带转义的字符串无法零拷贝借用，会反序列化失败
                let s = String::deserialize(d)?;
                Ok(Self::from(s.as_str()))
            }
        }
    };
}

str_enum_serde!(BodyType);
str_enum_serde!(AuthType);
str_enum_serde!(AssertOp);

#[cfg(test)]
mod tests {
    use super::*;

    /// 认不出的枚举值必须回落默认而非报错——手写 YAML 里一个拼错的 op
    /// 不该让整份 case 变成「解析失败」。
    #[test]
    fn unknown_enum_values_fall_back() {
        assert_eq!(AssertOp::from("equals"), AssertOp::Eq);
        assert_eq!(AuthType::from("jwt"), AuthType::None);
        assert_eq!(BodyType::from("raw"), BodyType::None);

        let op: AssertOp = serde_json::from_str("\"nope\"").expect("反序列化不应失败");
        assert_eq!(op, AssertOp::Eq);
    }

    /// 枚举经 JSON 往返后取值不变（前端 IPC 的形状契约）
    #[test]
    fn enum_json_roundtrip() {
        for op in [AssertOp::Eq, AssertOp::NotExists, AssertOp::Matches] {
            let s = serde_json::to_string(&op).unwrap();
            assert_eq!(serde_json::from_str::<AssertOp>(&s).unwrap(), op);
        }
        assert_eq!(serde_json::to_string(&AssertOp::NotExists).unwrap(), "\"notExists\"");
        assert_eq!(serde_json::to_string(&BodyType::FormUrlencoded).unwrap(), "\"form-urlencoded\"");
    }

    /// 空名行不参与发送——编辑器里的"下一行占位"不该被发出去
    #[test]
    fn blank_rows_are_inactive() {
        assert!(Kv::new("a", "1").active());
        assert!(!Kv::new("  ", "1").active());
        let mut k = Kv::new("a", "1");
        k.enabled = false;
        assert!(!k.active());
    }
}
