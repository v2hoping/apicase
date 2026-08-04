//! case / application.yml 的解析与序列化 —— **格式的唯一权威实现**。
//!
//! 桌面端与将来的 CLI 都从这里读写 case，因此不存在"界面能打开、CLI 读不了"这类
//! 两份解析器必然产生的漂移。前端 `case.ts` 已退化为类型定义 + IPC 封装。
//!
//! # 容错是硬要求
//!
//! case 与 `application.yml` 都是**手写**文件。一个拼错的字段、一个类型写反的值，
//! 不该让整个功能瘫掉——所以除了「YAML 本身语法错」之外，一律回落到安全默认值：
//! 认不出的 `op` 当 `eq`、认不出的 `auth.type` 当 `none`、`settings` 解析失败当全默认。
//! 唯一会向上报错的是 `parse_case` 的语法错误，因为那时连"哪一行"都定位不了。

mod emit;

pub use emit::to_yaml;

use crate::model::*;
use serde_json::{json, Map, Value};

// ── serde_yaml → serde_json ─────────────────────────

/// YAML 值转 JSON 值。
///
/// 非字符串的 mapping key（`1: x`、`true: y`）转成其字面文本——JSON 的 key 只能是字符串，
/// 而丢弃这类键会让用户的数据凭空消失。case 的 schema 里不会出现它们，
/// 但 `vars` 与 `body.json` 是用户的自由结构。
fn yaml_to_json(v: serde_yaml::Value) -> Value {
    match v {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(b),
        serde_yaml::Value::Number(n) => n
            .as_i64()
            .map(Value::from)
            .or_else(|| n.as_u64().map(Value::from))
            .or_else(|| n.as_f64().and_then(serde_json::Number::from_f64).map(Value::Number))
            .unwrap_or(Value::Null),
        serde_yaml::Value::String(s) => Value::String(s),
        serde_yaml::Value::Sequence(seq) => Value::Array(seq.into_iter().map(yaml_to_json).collect()),
        serde_yaml::Value::Mapping(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                out.insert(yaml_key_to_string(k), yaml_to_json(val));
            }
            Value::Object(out)
        }
        // `!tag value` —— 标签本身对 case 没有语义，取其值继续
        serde_yaml::Value::Tagged(t) => yaml_to_json(t.value),
    }
}

fn yaml_key_to_string(k: serde_yaml::Value) -> String {
    match k {
        serde_yaml::Value::String(s) => s,
        other => match yaml_to_json(other) {
            Value::String(s) => s,
            Value::Null => String::new(),
            v => v.to_string(),
        },
    }
}

/// 解析成 JSON 值；空文档按空映射处理（与前端 `load(text) ?? {}` 一致）。
fn load(text: &str) -> Result<Value, String> {
    let v: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| format!("YAML 解析失败：{e}"))?;
    Ok(match yaml_to_json(v) {
        Value::Null => Value::Object(Map::new()),
        other => other,
    })
}

// ── 取值工具 ────────────────────────────────────────

fn obj(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

/// 标量转字符串：null 与缺失都是空串，结构体走 JSON 文本。
/// 对应前端 `case.ts` 的 `str()`——YAML 里 `value: 200` 写成数字是常事，
/// 而模型里 `Kv.value` 恒是字符串，这里统一收口。
fn s(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

fn field<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a Value> {
    m.get(k)
}

fn sf(m: &Map<String, Value>, k: &str) -> String {
    s(field(m, k))
}

/// 空串归 None——`description: ''` 与没写 description 是一回事，不该在模型里留个空壳。
///
/// 走 `s()` 而不是只认 `Value::String`：schema 说这个字段是字符串，那么用户裸写
/// `description: 123` 就该读成 `"123"`，而不是被当成"类型不对"悄悄丢掉。
/// 这条对所有 schema 固定为字符串的字段一致——正因如此，序列化侧才敢不加引号。
fn opt_str(m: &Map<String, Value>, k: &str) -> Option<String> {
    let v = sf(m, k);
    (!v.is_empty()).then_some(v)
}

// ── 解析：YAML → 模型 ───────────────────────────────

fn norm_kv_row(m: &Map<String, Value>) -> Kv {
    Kv {
        name: sf(m, "name"),
        value: sf(m, "value"),
        // 只有显式 false 才禁用；缺失或写错类型都按启用兜底
        enabled: field(m, "enabled") != Some(&Value::Bool(false)),
        description: opt_str(m, "description"),
    }
}

fn norm_kv(v: Option<&Value>) -> Vec<Kv> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(obj).map(norm_kv_row).collect())
        .unwrap_or_default()
}

fn norm_form(v: Option<&Value>) -> Vec<FormItem> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(obj)
                .map(|m| {
                    let row = norm_kv_row(m);
                    FormItem {
                        name: row.name,
                        value: row.value,
                        enabled: row.enabled,
                        description: row.description,
                        // 只认 `type: file`，其余一律文本（且不写回该字段）
                        kind: (sf(m, "type") == "file").then_some(FormKind::File),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn norm_auth(v: Option<&Value>) -> AuthSpec {
    let Some(m) = v.and_then(obj) else {
        return AuthSpec::default();
    };
    let kind = AuthType::from(sf(m, "type").as_str());
    let sub = |k: &str| field(m, k).and_then(obj);
    let mut out = AuthSpec { kind, ..Default::default() };
    match kind {
        AuthType::Bearer => {
            out.bearer = Some(BearerAuth { token: sub("bearer").map(|b| sf(b, "token")).unwrap_or_default() });
        }
        AuthType::Basic => {
            let b = sub("basic");
            out.basic = Some(BasicAuth {
                username: b.map(|b| sf(b, "username")).unwrap_or_default(),
                password: b.map(|b| sf(b, "password")).unwrap_or_default(),
            });
        }
        AuthType::Apikey => {
            let k = sub("apikey");
            out.apikey = Some(ApikeyAuth {
                key: k.map(|k| sf(k, "key")).unwrap_or_default(),
                value: k.map(|k| sf(k, "value")).unwrap_or_default(),
                r#in: if k.map(|k| sf(k, "in")).as_deref() == Some("query") {
                    ApikeyIn::Query
                } else {
                    ApikeyIn::Header
                },
            });
        }
        AuthType::Digest => {
            let d = sub("digest");
            out.digest = Some(DigestAuth {
                username: d.map(|d| sf(d, "username")).unwrap_or_default(),
                password: d.map(|d| sf(d, "password")).unwrap_or_default(),
            });
        }
        AuthType::Oauth2 => {
            let o = sub("oauth2");
            out.oauth2 = Some(Oauth2Auth {
                token_url: o.map(|o| sf(o, "tokenUrl")).unwrap_or_default(),
                client_id: o.map(|o| sf(o, "clientId")).unwrap_or_default(),
                client_secret: o.map(|o| sf(o, "clientSecret")).unwrap_or_default(),
                scope: o.and_then(|o| {
                    let v = sf(o, "scope");
                    (!v.is_empty()).then_some(v)
                }),
                client_auth: if o.map(|o| sf(o, "clientAuth")).as_deref() == Some("body") {
                    ClientAuth::Body
                } else {
                    ClientAuth::Header
                },
            });
        }
        AuthType::None => {}
    }
    out
}

fn norm_body(v: Option<&Value>) -> BodySpec {
    let Some(m) = v.and_then(obj) else {
        return BodySpec::default();
    };
    let kind = BodyType::from(sf(m, "type").as_str());
    let ct = opt_str(m, "contentType");
    let mut out = BodySpec { kind, ..Default::default() };
    match kind {
        // json 保留原值（含 null——用户可能就是要发一个 JSON null）
        BodyType::Json => out.json = field(m, "json").cloned(),
        BodyType::Xml => out.xml = Some(sf(m, "xml")),
        BodyType::Text => {
            out.text = Some(sf(m, "text"));
            out.content_type = ct;
        }
        BodyType::FormUrlencoded => out.urlencoded = Some(norm_kv(field(m, "urlencoded"))),
        BodyType::FormData => out.form_data = Some(norm_form(field(m, "formData"))),
        BodyType::Binary => {
            out.file_path = Some(sf(m, "filePath"));
            out.content_type = ct;
        }
        BodyType::None => {}
    }
    out
}

fn norm_http(v: Option<&Value>) -> HttpSpec {
    let empty = Map::new();
    let m = v.and_then(obj).unwrap_or(&empty);
    let method = sf(m, "method");
    HttpSpec {
        method: if method.is_empty() { "GET".into() } else { method.to_uppercase() },
        url: sf(m, "url"),
        query: norm_kv(field(m, "query")),
        headers: norm_kv(field(m, "headers")),
        auth: norm_auth(field(m, "auth")),
        body: norm_body(field(m, "body")),
    }
}

/// `outputs: { token: $.data.token }` → 有序列表（YAML 里的书写顺序即展示顺序）
fn norm_outputs(v: Option<&Value>) -> Vec<StepOutput> {
    v.and_then(obj)
        .map(|m| m.iter().map(|(k, p)| StepOutput { name: k.clone(), path: s(Some(p)) }).collect())
        .unwrap_or_default()
}

fn norm_assertions(v: Option<&Value>) -> Vec<Assertion> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(obj)
                .map(|m| Assertion {
                    target: sf(m, "target"),
                    op: AssertOp::from(sf(m, "op").as_str()),
                    value: match field(m, "value") {
                        None | Some(Value::Null) => None,
                        other => Some(s(other)),
                    },
                })
                // 没有 target 的断言无从评估，丢弃（编辑器里的空占位行）
                .filter(|a| !a.target.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn norm_step(v: &Value, i: usize) -> Step {
    let empty = Map::new();
    let m = obj(v).unwrap_or(&empty);
    let id = sf(m, "id");
    let protocol = sf(m, "protocol");
    Step {
        id: if id.is_empty() { format!("step{}", i + 1) } else { id },
        protocol: if protocol.is_empty() { "http".into() } else { protocol },
        ui: norm_step_ui(field(m, "ui")),
        http: norm_http(field(m, "request")),
        depends_on: field(m, "dependsOn")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|x| s(Some(x))).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default(),
        outputs: norm_outputs(field(m, "outputs")),
        assertions: norm_assertions(field(m, "assertions")),
        docs: opt_str(m, "docs"),
    }
}

/// step 的前端属性。坐标必须是数字：写坏了当作没有（回退自动布局），
/// 而不是让整个 step 解析失败——坐标是视图态，不该有能力废掉一条用例。
fn norm_step_ui(v: Option<&Value>) -> Option<StepUi> {
    let m = obj(v?)?;
    let x = m.get("x").and_then(Value::as_f64)?;
    let y = m.get("y").and_then(Value::as_f64)?;
    Some(StepUi { x, y })
}

/// 解析 case 文本。**唯一格式**：顶层 `steps:` 列表（单节点 = 长度 1，每步含
/// `protocol:` 与 `request:`）。旧格式（`requests:` 列表、`http:` 报文键）不再兼容。
pub fn parse_case(text: &str) -> Result<Case, String> {
    let v = load(text)?;
    let empty = Map::new();
    let m = obj(&v).unwrap_or(&empty);
    Ok(Case {
        // 走 s() 而不是只认 String：老文件里写成 `apicase: 0.1`（YAML 读成数字）
        // 也要能读出 "0.1"，而不是当成"类型不对"回落默认
        version: {
            let v = sf(m, "apicase");
            if v.is_empty() { CASE_VERSION.into() } else { v }
        },
        name: opt_str(m, "name"),
        vars: field(m, "vars").and_then(obj).cloned(),
        requests: field(m, "steps")
            .and_then(Value::as_array)
            .map(|a| a.iter().enumerate().map(|(i, s)| norm_step(s, i)).collect())
            .unwrap_or_default(),
    })
}

/// `analyze_case` 的结果——对前端序列化为 `{ valid, case?, error? }`。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case: Option<Case>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AnalyzeResult {
    fn invalid(msg: impl Into<String>) -> Self {
        Self { valid: false, case: None, error: Some(msg.into()) }
    }
}

/// 校验并解析 case 文本，用于「内容驱动默认视图 / 文本兜底」。
///
/// `valid=true` 仅当能解析成对象且含 `steps:` 列表。判定为无效时前端回退纯文本编辑，
/// 因此**这里每多认一种形态，就多一类会被结构化编辑器改写的文件**——从严。
pub fn analyze_case(text: &str) -> AnalyzeResult {
    let v = match load(text) {
        Ok(v) => v,
        Err(e) => return AnalyzeResult::invalid(e),
    };
    let Some(m) = obj(&v) else {
        return AnalyzeResult::invalid("顶层不是对象，不是有效的 case");
    };
    let Some(steps) = field(m, "steps").and_then(Value::as_array) else {
        return AnalyzeResult::invalid("缺少 steps 列表");
    };
    // 止损：steps 已是新格式，但某步仍用旧内层键 `http:` 而非 `request:`。
    // 判为无效、回退纯文本——否则结构化编辑器读到空报文，一保存就把原报文覆盖没了。
    let legacy = steps
        .iter()
        .filter_map(obj)
        .any(|s| s.contains_key("http") && !s.contains_key("request"));
    if legacy {
        return AnalyzeResult::invalid("step 使用了旧的 http: 报文键，请改为 request:");
    }
    match parse_case(text) {
        Ok(c) => AnalyzeResult { valid: true, case: Some(c), error: None },
        Err(e) => AnalyzeResult::invalid(e),
    }
}

// ── 序列化：模型 → YAML ─────────────────────────────
//
// 通篇裁剪默认值：`enabled: true`、空 body、`protocol` 之外的空列表都不落盘。
// 写出一堆等于默认值的字段，会让真正被改过的那一行淹没在 diff 里。

fn ser_kv(list: &[Kv]) -> Vec<Value> {
    list.iter()
        // 名字与值都空 = 编辑器里的占位行，不落盘
        .filter(|k| !k.name.trim().is_empty() || !k.value.trim().is_empty())
        .map(|k| {
            let mut o = Map::new();
            o.insert("name".into(), json!(k.name));
            o.insert("value".into(), json!(k.value));
            if !k.enabled {
                o.insert("enabled".into(), json!(false));
            }
            if let Some(d) = k.description.as_ref().filter(|d| !d.trim().is_empty()) {
                o.insert("description".into(), json!(d));
            }
            Value::Object(o)
        })
        .collect()
}

/// form-data 项：`type` 排在 `name` 与 `value` 之间——一眼能看出这行是文件还是文本。
fn ser_form(list: &[FormItem]) -> Vec<Value> {
    list.iter()
        .filter(|k| !k.name.trim().is_empty() || !k.value.trim().is_empty())
        .map(|k| {
            let mut o = Map::new();
            o.insert("name".into(), json!(k.name));
            if k.is_file() {
                o.insert("type".into(), json!("file"));
            }
            o.insert("value".into(), json!(k.value));
            if !k.enabled {
                o.insert("enabled".into(), json!(false));
            }
            if let Some(d) = k.description.as_ref().filter(|d| !d.trim().is_empty()) {
                o.insert("description".into(), json!(d));
            }
            Value::Object(o)
        })
        .collect()
}

fn ser_auth(a: &AuthSpec) -> Option<Value> {
    let mut o = Map::new();
    o.insert("type".into(), json!(a.kind.as_str()));
    match a.kind {
        AuthType::None => return None,
        AuthType::Bearer => {
            let t = a.bearer.as_ref().map(|b| b.token.clone()).unwrap_or_default();
            o.insert("bearer".into(), json!({ "token": t }));
        }
        AuthType::Basic => {
            let b = a.basic.clone().unwrap_or_default();
            o.insert("basic".into(), json!({ "username": b.username, "password": b.password }));
        }
        AuthType::Apikey => {
            let k = a.apikey.clone().unwrap_or_default();
            let mut km = Map::new();
            km.insert("key".into(), json!(k.key));
            km.insert("value".into(), json!(k.value));
            km.insert("in".into(), json!(if k.r#in == ApikeyIn::Query { "query" } else { "header" }));
            o.insert("apikey".into(), Value::Object(km));
        }
        AuthType::Digest => {
            let d = a.digest.clone().unwrap_or_default();
            o.insert("digest".into(), json!({ "username": d.username, "password": d.password }));
        }
        AuthType::Oauth2 => {
            let t = a.oauth2.clone().unwrap_or_default();
            let mut om = Map::new();
            om.insert("tokenUrl".into(), json!(t.token_url));
            om.insert("clientId".into(), json!(t.client_id));
            om.insert("clientSecret".into(), json!(t.client_secret));
            if let Some(sc) = t.scope.as_ref().filter(|s| !s.is_empty()) {
                om.insert("scope".into(), json!(sc));
            }
            // header 是默认值，不落盘
            if t.client_auth == ClientAuth::Body {
                om.insert("clientAuth".into(), json!("body"));
            }
            o.insert("oauth2".into(), Value::Object(om));
        }
    }
    Some(Value::Object(o))
}

fn ser_body(b: &BodySpec) -> Option<Value> {
    let mut o = Map::new();
    let ct = b.content_type.as_deref().filter(|s| !s.is_empty());
    match b.kind {
        BodyType::None => return None,
        BodyType::Json => {
            // 空 json 体等于没有 body——写个 `type: json` 空壳只会让人以为漏了内容
            let v = b.json.as_ref()?;
            if matches!(v, Value::Null) || v == &json!("") {
                return None;
            }
            o.insert("type".into(), json!("json"));
            o.insert("json".into(), v.clone());
        }
        BodyType::Xml => {
            let x = b.xml.as_deref().filter(|s| !s.is_empty())?;
            o.insert("type".into(), json!("xml"));
            o.insert("xml".into(), json!(x));
        }
        BodyType::Text => {
            let t = b.text.as_deref().filter(|s| !s.is_empty())?;
            o.insert("type".into(), json!("text"));
            if let Some(c) = ct {
                o.insert("contentType".into(), json!(c));
            }
            o.insert("text".into(), json!(t));
        }
        BodyType::Binary => {
            let p = b.file_path.as_deref().filter(|s| !s.is_empty())?;
            o.insert("type".into(), json!("binary"));
            if let Some(c) = ct {
                o.insert("contentType".into(), json!(c));
            }
            o.insert("filePath".into(), json!(p));
        }
        BodyType::FormUrlencoded => {
            let rows = ser_kv(b.urlencoded.as_deref().unwrap_or(&[]));
            if rows.is_empty() {
                return None;
            }
            o.insert("type".into(), json!("form-urlencoded"));
            o.insert("urlencoded".into(), Value::Array(rows));
        }
        BodyType::FormData => {
            let rows = ser_form(b.form_data.as_deref().unwrap_or(&[]));
            if rows.is_empty() {
                return None;
            }
            o.insert("type".into(), json!("form-data"));
            o.insert("formData".into(), Value::Array(rows));
        }
    }
    Some(Value::Object(o))
}

fn ser_http(h: &HttpSpec) -> Value {
    let mut o = Map::new();
    o.insert("method".into(), json!(h.method));
    o.insert("url".into(), json!(h.url));
    let q = ser_kv(&h.query);
    if !q.is_empty() {
        o.insert("query".into(), Value::Array(q));
    }
    let hs = ser_kv(&h.headers);
    if !hs.is_empty() {
        o.insert("headers".into(), Value::Array(hs));
    }
    if let Some(a) = ser_auth(&h.auth) {
        o.insert("auth".into(), a);
    }
    if let Some(b) = ser_body(&h.body) {
        o.insert("body".into(), b);
    }
    Value::Object(o)
}

// 顺序对齐文档示例：id → protocol → dependsOn → request → outputs → assertions → docs
fn ser_step(st: &Step) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(st.id));
    o.insert("protocol".into(), json!(if st.protocol.is_empty() { "http" } else { &st.protocol }));
    if let Some(u) = st.ui {
        o.insert("ui".into(), json!({ "x": num(u.x), "y": num(u.y) }));
    }
    if !st.depends_on.is_empty() {
        o.insert("dependsOn".into(), json!(st.depends_on));
    }
    o.insert("request".into(), ser_http(&st.http));
    let mut outs = Map::new();
    for it in &st.outputs {
        if !it.name.trim().is_empty() {
            outs.insert(it.name.trim().into(), json!(it.path));
        }
    }
    if !outs.is_empty() {
        o.insert("outputs".into(), Value::Object(outs));
    }
    let asserts: Vec<Value> = st
        .assertions
        .iter()
        .filter(|a| !a.target.trim().is_empty())
        .map(|a| {
            let mut am = Map::new();
            am.insert("target".into(), json!(a.target));
            am.insert("op".into(), json!(a.op.as_str()));
            if a.op.needs_value() {
                if let Some(v) = a.value.as_ref().filter(|v| !v.is_empty()) {
                    am.insert("value".into(), json!(v));
                }
            }
            Value::Object(am)
        })
        .collect();
    if !asserts.is_empty() {
        o.insert("assertions".into(), Value::Array(asserts));
    }
    if let Some(d) = st.docs.as_ref().filter(|d| !d.trim().is_empty()) {
        o.insert("docs".into(), json!(d));
    }
    Value::Object(o)
}

/// 画布坐标：整数值写成整数。`x: 502.0` 与 `x: 502` 语义相同，
/// 但后者才是人手写的样子——模型里坐标是 f64，不收这一道就会凭空多出一位小数。
fn num(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

/// case → YAML 文本（统一写 `steps:` 列表；单请求 = 长度 1）。
pub fn dump_case(c: &Case) -> String {
    let mut o = Map::new();
    o.insert("apicase".into(), json!(if c.version.is_empty() { CASE_VERSION } else { &c.version }));
    if let Some(n) = c.name.as_ref().filter(|n| !n.is_empty()) {
        o.insert("name".into(), json!(n));
    }
    if let Some(v) = c.vars.as_ref().filter(|v| !v.is_empty()) {
        o.insert("vars".into(), Value::Object(v.clone()));
    }
    o.insert("steps".into(), Value::Array(c.requests.iter().map(ser_step).collect()));
    to_yaml(&Value::Object(o))
}

// ── application.yml ─────────────────────────────────

/// 一套环境的变量表。字母序（`BTreeMap`）而非书写序：报告里要展示它，
/// 顺序稳定才能让两次运行的报告可比对。
pub type Vars = std::collections::BTreeMap<String, String>;
/// 环境名 → 变量表。用有序 JSON 对象承载，保住 `application.yml` 里的书写顺序
/// （环境下拉按此顺序展示，dev/test/prod 的排列是作者的意图）。
pub type Environments = Map<String, Value>;

/// 从 `application.yml` 文本解析 environment：`{ 环境名: { 变量: 值 } }`（值统一转字符串）。
/// 解析失败返回空表而不是报错——配置写坏了不该让环境下拉直接消失。
pub fn parse_environments(text: &str) -> Environments {
    let Ok(v) = load(text) else { return Map::new() };
    let Some(env) = v.as_object().and_then(|m| m.get("environment")).and_then(Value::as_object) else {
        return Map::new();
    };
    let mut out = Map::new();
    for (name, vars) in env {
        let mut m = Map::new();
        if let Some(vo) = vars.as_object() {
            for (k, val) in vo {
                m.insert(k.clone(), json!(s(Some(val))));
            }
        }
        out.insert(name.clone(), Value::Object(m));
    }
    out
}

/// 取某套环境的变量表（环境不存在即空表）。
pub fn env_vars(envs: &Environments, name: &str) -> Vars {
    let mut out = Vars::new();
    if let Some(m) = envs.get(name).and_then(Value::as_object) {
        for (k, v) in m {
            out.insert(k.clone(), s(Some(v)));
        }
    }
    out
}

/// 从 `application.yml` 文本解析 `settings:`。
/// 容错同 `parse_environments`：解析失败 / 键缺失 / 类型不符一律回落默认，绝不抛错。
pub fn parse_settings(text: &str) -> WorkspaceSettings {
    let d = WorkspaceSettings::default();
    let Ok(v) = load(text) else { return d };
    let Some(st) = v.as_object().and_then(|m| m.get("settings")).and_then(Value::as_object) else {
        return d;
    };
    // 超时：非数字 / 负数 / 非有限值一律回 0（不限制），小数取整
    let timeout_ms = match st.get("timeoutMs") {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite() && *f > 0.0).map(|f| f as u64).unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite() && *f > 0.0).map(|f| f as u64).unwrap_or(0),
        _ => 0,
    };
    WorkspaceSettings {
        // 只有显式 false 才关闭：键缺失或写错类型都按「校验开启」这一安全侧兜底
        verify_ssl: st.get("verifySsl") != Some(&Value::Bool(false)),
        use_custom_ca: st.get("useCustomCa") == Some(&Value::Bool(true)),
        ca_cert: s(st.get("caCert")).trim().to_string(),
        timeout_ms,
        // 同 verifySsl：只有显式 false 才关闭。默认自动收发 cookie（对齐 Postman / Bruno）
        cookies: st.get("cookies") != Some(&Value::Bool(false)),
        // 同样只有显式 true 才启用：默认（阻断）是更安全的一侧——
        // 上游挂了还往下发真实写请求，后果比多跳几个节点严重
        continue_on_assertion_failure: st.get("continueOnAssertionFailure") == Some(&Value::Bool(true)),
    }
}

/// 可视化设置接管的键。**每加一个设置项都必须列进来**——写回时先清掉这些键再写
/// `ser_settings` 的产出，而默认值不落盘，漏了的那个键就再也没人去覆盖它：
/// 表现为「界面上关掉了，文件里仍是 true，重新打开又变回开」。
/// （`continueOnAssertionFailure` 曾漏在这里，本期一并补上。）
const MANAGED_SETTING_KEYS: [&str; 6] =
    ["verifySsl", "useCustomCa", "caCert", "timeoutMs", "cookies", "continueOnAssertionFailure"];

/// settings → YAML 映射；全为默认时返回 None，调用方据此整键不落盘。
fn ser_settings(s: &WorkspaceSettings) -> Option<Map<String, Value>> {
    let mut o = Map::new();
    if !s.verify_ssl {
        o.insert("verifySsl".into(), json!(false));
    }
    if s.use_custom_ca {
        o.insert("useCustomCa".into(), json!(true));
    }
    if !s.ca_cert.trim().is_empty() {
        o.insert("caCert".into(), json!(s.ca_cert.trim()));
    }
    if s.timeout_ms > 0 {
        o.insert("timeoutMs".into(), json!(s.timeout_ms));
    }
    if !s.cookies {
        o.insert("cookies".into(), json!(false));
    }
    if s.continue_on_assertion_failure {
        o.insert("continueOnAssertionFailure".into(), json!(true));
    }
    (!o.is_empty()).then_some(o)
}

/// 把可视化编辑的 environment / settings 写回 `application.yml`。
///
/// 保留原文的其它顶层键（注释不可避免地丢失——YAML 库都不保留）。
/// `settings` 为 None 时不动原文该键；全为默认值则整键删除。
pub fn dump_application_config(
    base_text: &str,
    environment: &Environments,
    settings: Option<&WorkspaceSettings>,
) -> String {
    let mut base = load(base_text)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    base.insert("environment".into(), Value::Object(environment.clone()));
    if let Some(st) = settings {
        // 只接管自己认得的那几个键：原文里其它手写键（未来字段 / 用户自定义）原样保留，
        // 否则一次可视化保存就会把它们悄悄吃掉。
        let mut prev = base.get("settings").and_then(Value::as_object).cloned().unwrap_or_default();
        for k in MANAGED_SETTING_KEYS {
            prev.remove(k);
        }
        if let Some(next) = ser_settings(st) {
            for (k, v) in next {
                prev.insert(k, v);
            }
        }
        if prev.is_empty() {
            base.remove("settings");
        } else {
            base.insert("settings".into(), Value::Object(prev));
        }
    }
    to_yaml(&Value::Object(base))
}

#[cfg(test)]
mod tests;
