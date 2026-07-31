//! 报告脱敏与报文体截断。
//!
//! # 脱敏是默认行为，不是可选项
//!
//! 报告会被转发、进 CI 制品、甚至被提交进仓库。"报告工具泄露线上 token"
//! 是业界反复发生过的事故，所以默认开启、要关得显式去关。
//!
//! 五条规则互补，缺一不可：
//!
//! 1. **按名字掩码请求头**——`Authorization` 这类头，值本身就是凭据；
//! 2. **已知凭据值的字面替换**——凭据会被回显到响应体 / URL 的任何位置
//!    （httpbin 的 `/get` 直接回显请求头；API Key 放 query 时凭据本就在 URL 里）；
//! 3. **JSON 响应体按 key 掩码**——登录接口**新返回**的 token 事先无从得知其值，
//!    只能靠字段名识别，规则 2 对它无能为力；
//! 4. **`outputs` 在报告里掩码**，但**透传给下游 step 的是原值**——
//!    下游要拿它发真实请求；
//! 5. **断言的实际值 / 期望值**——`$.data.token exists` 这条断言的 `actual`
//!    就是 token 原文，前四条一条都拦不住它。
//!
//! **顺序上先脱敏再截断**：反过来的话，横跨截断边界的凭据会被切掉尾巴、
//! 前半截留在报告里——那和没脱敏差不多。

use crate::report::{AssertRecord, BodyRecord, KvPair};
use crate::util::js_string;
use serde_json::Value;
use std::collections::BTreeMap;

/// 整头掩码的头名（值本身即凭据）。
const SECRET_HEADERS: &[&str] =
    &["authorization", "proxy-authorization", "cookie", "set-cookie", "x-api-key", "api-key"];

/// 名字里带这些词的头 / 变量 / 字段一律视作凭据。
const SECRET_WORDS: &[&str] =
    &["token", "secret", "password", "passwd", "credential", "auth", "apikey", "api-key", "api_key"];

/// 这个名字（头名 / 变量名 / JSON 字段名）是否该被掩码。
pub fn is_secret_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    if SECRET_HEADERS.contains(&n.as_str()) {
        return true;
    }
    SECRET_WORDS.iter().any(|w| n.contains(w))
}

/// 掩码一个凭据值。
///
/// 保留 scheme（`Bearer` / `Basic`）与值的前 4 位——完全抹掉会让
/// 「是不是根本没带上认证」「是不是带了个空 token」这类问题无从排查，
/// 而这恰恰是看报告时最常问的两个问题。
pub fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    // `Bearer eyJ…` —— 前缀短且带空格的当作 scheme 保留
    if let Some(sp) = value.find(' ') {
        if sp > 0 && sp <= 10 {
            return format!("{} {}", &value[..sp], mask_tail(&value[sp + 1..]));
        }
    }
    mask_tail(value)
}

fn mask_tail(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    // 太短的值留前 4 位等于没掩——整个抹掉
    if s.chars().count() <= 4 {
        return "***".into();
    }
    let head: String = s.chars().take(4).collect();
    format!("{head}***")
}

/// 按需脱敏一组头。`redact=false` 时原样返回。
pub fn redact_headers(headers: &[KvPair], redact: bool) -> Vec<KvPair> {
    if !redact {
        return headers.to_vec();
    }
    headers
        .iter()
        .map(|h| {
            if is_secret_name(&h.key) {
                KvPair::new(&h.key, mask_secret(&h.value))
            } else {
                h.clone()
            }
        })
        .collect()
}

/// 按需脱敏一组变量（environment 会整份写进报告头部）。
pub fn redact_vars(vars: &BTreeMap<String, String>, redact: bool) -> BTreeMap<String, String> {
    vars.iter()
        .map(|(k, v)| {
            let v = if redact && is_secret_name(k) { mask_secret(v) } else { v.clone() };
            (k.clone(), v)
        })
        .collect()
}

/// `outputs` 在报告里的形态：名字像凭据的掩码掉
/// （登录 step 的 `token` 输出是最典型的一个）。
pub fn redact_outputs(outputs: &BTreeMap<String, Value>, redact: bool) -> BTreeMap<String, Value> {
    if !redact {
        return outputs.clone();
    }
    outputs
        .iter()
        .map(|(k, v)| {
            let v = match v {
                Value::String(s) if is_secret_name(k) => Value::String(mask_secret(s)),
                other => other.clone(),
            };
            (k.clone(), v)
        })
        .collect()
}

/// 断言结果里的实际值 / 期望值。
///
/// **这是最容易漏掉的一处**：一条 `$.data.token exists` 断言，`actual` 里放的
/// 就是刚从响应体里取出来的 token 原文——而它既不在请求头里、也不在 outputs 里，
/// 前面三条规则一条都拦不住它。
///
/// 两道：目标路径像凭据（`$.data.token`、`header.Authorization`）就按名字掩码；
/// 否则仍按已知凭据值做字面替换（断言可能取到别处回显的凭据）。
pub fn redact_assertions(list: Vec<AssertRecord>, secrets: &[String], redact: bool) -> Vec<AssertRecord> {
    if !redact {
        return list;
    }
    list.into_iter()
        .map(|mut a| {
            if is_secret_target(&a.target) {
                a.actual = mask_display(&a.actual);
                a.expected = mask_display(&a.expected);
            } else {
                a.actual = scrub_secrets(&a.actual, secrets);
                a.expected = scrub_secrets(&a.expected, secrets);
            }
            a
        })
        .collect()
}

/// 断言目标是否指向凭据——取路径最后一段来判断。
/// `$.data.token` / `header.Authorization` → 是；`status` / `$.args.page` → 否。
fn is_secret_target(target: &str) -> bool {
    let last = target
        .trim_end_matches([']', '\'', '"'])
        .rsplit(['.', '[', '\'', '"'])
        .find(|s| !s.is_empty())
        .unwrap_or(target);
    is_secret_name(last)
}

/// 掩码一个展示值，但**放过两个占位符**：`∅`（路径不存在）与 `—`（本操作符无期望值）。
/// 把它们也掩成 `***` 会让"到底是没这个字段还是值被藏了"变得无从分辨，
/// 而这恰恰是看失败断言时最要紧的一点信息。
fn mask_display(v: &str) -> String {
    if v == "∅" || v == "—" || v.is_empty() {
        return v.to_string();
    }
    mask_secret(v)
}

/// 值长度下限。太短的值（`"1"`、`"dev"`）拿去做全局字面替换，
/// 会把正常内容打得千疮百孔——一个 `"1"` 能把整份 JSON 里所有的 1 都换掉。
const MIN_SECRET_LEN: usize = 6;

/// 从变量表里挑出**已知的凭据字面值**，供报文体 / URL 的字面替换用。
pub fn secret_values<'a, I>(entries: I, redact: bool) -> Vec<String>
where
    I: IntoIterator<Item = (&'a String, &'a Value)>,
{
    if !redact {
        return Vec::new();
    }
    entries
        .into_iter()
        .filter(|(k, _)| is_secret_name(k))
        .filter_map(|(_, v)| match v {
            Value::String(s) if s.len() >= MIN_SECRET_LEN => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// 字面替换已知凭据值（用于 URL 与报文体）。
pub fn scrub_secrets(text: &str, secrets: &[String]) -> String {
    if text.is_empty() || secrets.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for s in secrets {
        if !s.is_empty() && out.contains(s.as_str()) {
            out = out.replace(s.as_str(), "***");
        }
    }
    out
}

const MAX_SCRUB_DEPTH: usize = 12;

/// JSON 报文体按 key 掩码。
///
/// 与 `scrub_secrets` 互补：后者只认**我们已知**的凭据值，而登录接口**新返回**的
/// token 事先无从得知其值，只能靠字段名识别。非 JSON 或解析失败一律原样返回
/// （不猜结构——猜错会把用户的响应体改得面目全非）。
pub fn scrub_json_body(text: &str, redact: bool) -> String {
    if !redact || text.is_empty() {
        return text.to_string();
    }
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return text.to_string();
    }
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    let mut touched = false;
    let next = walk(&parsed, 0, &mut touched);
    if !touched {
        // 没命中就保留原文本（含缩进与键序）——重新序列化会平白改动报文体的样子
        return text.to_string();
    }
    serde_json::to_string(&next).unwrap_or_else(|_| text.to_string())
}

fn walk(v: &Value, depth: usize, touched: &mut bool) -> Value {
    if depth > MAX_SCRUB_DEPTH {
        return v.clone();
    }
    match v {
        Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, depth + 1, touched)).collect()),
        Value::Object(m) => {
            let mut out = serde_json::Map::with_capacity(m.len());
            for (k, val) in m {
                match val {
                    Value::String(s) if is_secret_name(k) && !s.is_empty() => {
                        out.insert(k.clone(), Value::String(mask_secret(s)));
                        *touched = true;
                    }
                    other => {
                        out.insert(k.clone(), walk(other, depth + 1, touched));
                    }
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

// ── 截断 ────────────────────────────────────────────

/// 截断报文体到 `max_bytes` 以内。
///
/// 单文件 HTML 会把这些内容全内联，几十个大 JSON 就能把报告顶到浏览器打不开的体积。
/// 切点落在**字符边界**上，不会切出半个 UTF-8 字符；`bytes` 记的是原始大小，
/// 截断后仍能看出响应到底多大。
pub fn clip_body(body: Option<&str>, max_bytes: usize) -> BodyRecord {
    let Some(s) = body else {
        return BodyRecord::absent();
    };
    let bytes = s.len();
    if bytes <= max_bytes {
        return BodyRecord { preview: Some(s.to_string()), bytes, truncated: false };
    }
    // 从上限位置向前退到最近的字符边界（最多退 3 个字节）
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    BodyRecord { preview: Some(s[..end].to_string()), bytes, truncated: true }
}

/// 报文体的完整清洗链：**先脱敏、后截断**（见模块文档）。
pub fn clean_body(body: Option<&str>, secrets: &[String], redact: bool, max_bytes: usize) -> BodyRecord {
    let Some(s) = body else {
        return BodyRecord::absent();
    };
    let scrubbed = scrub_secrets(&scrub_json_body(s, redact), secrets);
    clip_body(Some(&scrubbed), max_bytes)
}

/// 从一组变量里收集凭据字面值（`BTreeMap<String, String>` 版，供 environment 用）。
pub fn secret_values_of_strings(vars: &BTreeMap<String, String>, redact: bool) -> Vec<String> {
    if !redact {
        return Vec::new();
    }
    vars.iter()
        .filter(|(k, _)| is_secret_name(k))
        .filter(|(_, v)| v.len() >= MIN_SECRET_LEN)
        .map(|(_, v)| v.clone())
        .collect()
}

/// 从 outputs 里收集凭据字面值（值可能不是字符串，统一按文本形态收）。
pub fn secret_values_of_outputs(outputs: &BTreeMap<String, Value>, redact: bool) -> Vec<String> {
    if !redact {
        return Vec::new();
    }
    outputs
        .iter()
        .filter(|(k, _)| is_secret_name(k))
        .filter_map(|(_, v)| match v {
            Value::String(s) if s.len() >= MIN_SECRET_LEN => Some(s.clone()),
            Value::String(_) | Value::Null => None,
            other => {
                let t = js_string(other);
                (t.len() >= MIN_SECRET_LEN).then_some(t)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognizes_secret_names() {
        for n in ["Authorization", "cookie", "Set-Cookie", "X-Api-Key", "api_key", "accessToken", "MY_SECRET", "password", "userAuth"] {
            assert!(is_secret_name(n), "{n} 应被识别为凭据");
        }
        for n in ["Content-Type", "user", "name", "X-Trace-Id", "count"] {
            assert!(!is_secret_name(n), "{n} 不该被识别为凭据");
        }
    }

    /// 保留 scheme 与前 4 位：既不泄露，又能看出"到底带没带上认证"
    #[test]
    fn masks_but_keeps_a_usable_hint() {
        assert_eq!(mask_secret("Bearer eyJhbGciOiJIUzI1NiJ9"), "Bearer eyJh***");
        assert_eq!(mask_secret("Basic QWxhZGRpbg=="), "Basic QWxh***");
        assert_eq!(mask_secret("abcdefghij"), "abcd***");
        assert_eq!(mask_secret("abcd"), "***", "太短的留前 4 位等于没掩");
        assert_eq!(mask_secret(""), "", "空值仍是空——能看出根本没带凭据");
        // 中文不能被切成半个字符
        assert_eq!(mask_secret("密码密码密码"), "密码密码***");
    }

    #[test]
    fn redacts_headers_and_vars() {
        let hs = vec![KvPair::new("Authorization", "Bearer abcdefgh"), KvPair::new("Content-Type", "application/json")];
        let out = redact_headers(&hs, true);
        assert_eq!(out[0].value, "Bearer abcd***");
        assert_eq!(out[1].value, "application/json", "非凭据头原样");
        assert_eq!(redact_headers(&hs, false), hs, "关掉脱敏就原样返回");

        let vars: BTreeMap<String, String> =
            [("token".to_string(), "abcdefgh".to_string()), ("baseUrl".to_string(), "http://x".to_string())]
                .into_iter()
                .collect();
        let r = redact_vars(&vars, true);
        assert_eq!(r.get("token").unwrap(), "abcd***");
        assert_eq!(r.get("baseUrl").unwrap(), "http://x");
    }

    /// 凭据会被回显到响应体的任何位置——只按字段名掩码是挡不住的
    #[test]
    fn scrubs_known_secret_values_anywhere() {
        let secrets = vec!["s3cr3t-token-value".to_string()];
        let body = r#"{"echo":{"headers":{"Authorization":"Bearer s3cr3t-token-value"}}}"#;
        let out = scrub_secrets(body, &secrets);
        assert!(!out.contains("s3cr3t-token-value"), "{out}");
        assert!(out.contains("***"), "{out}");
        // URL 里的也要清（API Key 放 query 时凭据就在这）
        assert_eq!(scrub_secrets("http://x?k=s3cr3t-token-value", &secrets), "http://x?k=***");
    }

    /// 太短的值不参与字面替换，否则会把正常内容打成筛子
    #[test]
    fn short_values_are_not_used_for_literal_scrubbing() {
        let vars: BTreeMap<String, String> =
            [("token".to_string(), "abc".to_string()), ("secret".to_string(), "longenough".to_string())]
                .into_iter()
                .collect();
        let s = secret_values_of_strings(&vars, true);
        assert_eq!(s, vec!["longenough".to_string()]);
    }

    /// 登录接口**新返回**的 token 事先不知道值，只能靠字段名识别
    #[test]
    fn scrubs_json_response_by_field_name() {
        let body = r#"{"data":{"access_token":"brandnewtoken","user":"alice"},"nested":[{"password":"p@ssw0rd"}]}"#;
        let out = scrub_json_body(body, true);
        assert!(!out.contains("brandnewtoken"), "{out}");
        assert!(!out.contains("p@ssw0rd"), "{out}");
        assert!(out.contains("alice"), "非凭据字段保留：{out}");
        assert!(out.contains("bran***"), "{out}");
    }

    /// 没命中就保持原文本（含缩进与键序）——不该平白改动用户的响应体样子
    #[test]
    fn untouched_json_keeps_its_original_text() {
        let pretty = "{\n  \"b\": 1,\n  \"a\": 2\n}";
        assert_eq!(scrub_json_body(pretty, true), pretty);
        // 非 JSON 一律原样
        assert_eq!(scrub_json_body("<html>token</html>", true), "<html>token</html>");
        assert_eq!(scrub_json_body("{坏 JSON", true), "{坏 JSON");
        assert_eq!(scrub_json_body(r#"{"token":"x"}"#, false), r#"{"token":"x"}"#, "关掉脱敏就原样");
    }

    /// 深度上限：畸形的深嵌套结构不该把栈打爆
    #[test]
    fn deep_nesting_is_bounded() {
        let mut v = json!({"token": "abcdefgh"});
        for _ in 0..50 {
            v = json!({ "n": v });
        }
        let out = scrub_json_body(&v.to_string(), true);
        assert!(out.contains("abcdefgh"), "超过深度上限的部分保持原样，但不该崩");
    }

    #[test]
    fn clips_on_char_boundaries() {
        // 每个中文 3 字节；上限 7 只能容下 2 个字（6 字节）
        let r = clip_body(Some("中文中文"), 7);
        assert_eq!(r.preview.as_deref(), Some("中文"));
        assert!(r.truncated);
        assert_eq!(r.bytes, 12, "bytes 记的是原始大小");

        let r = clip_body(Some("short"), 100);
        assert_eq!(r.preview.as_deref(), Some("short"));
        assert!(!r.truncated);

        // "没有报文体" 与 "空报文体" 要能区分
        assert_eq!(clip_body(None, 10).preview, None);
        assert_eq!(clip_body(Some(""), 10).preview.as_deref(), Some(""));
    }

    /// **先脱敏再截断**：反过来的话，横跨边界的凭据会留下前半截
    #[test]
    fn scrubbing_happens_before_clipping() {
        let secret = "SUPERSECRETVALUE".to_string();
        let body = format!("prefix-{secret}-suffix");
        // 上限刚好切在凭据中间
        let r = clean_body(Some(&body), std::slice::from_ref(&secret), true, 12);
        let preview = r.preview.unwrap();
        assert!(!preview.contains("SUPER"), "凭据的任何片段都不该留下：{preview}");
        assert!(r.truncated);
    }

    /// 断言的 actual 直接来自响应体——一条 `$.data.token exists` 就能把 token
    /// 原文写进报告，而请求头、outputs、响应体三处的规则都拦不住它。
    #[test]
    fn assertion_values_are_redacted() {
        let rec = |target: &str, actual: &str, expected: &str| AssertRecord {
            target: target.into(),
            op: "eq".into(),
            expected: expected.into(),
            actual: actual.into(),
            ok: true,
        };
        let out = redact_assertions(
            vec![
                rec("$.data.token", "S3CR3T-TOKEN-abcdef", "—"),
                rec("header.Authorization", "Bearer abcdefgh", "—"),
                rec("status", "200", "200"),
                rec("$.args.page", "2", "2"),
                rec("$.data.token", "∅", "—"),
            ],
            &[],
            true,
        );
        assert_eq!(out[0].actual, "S3CR***", "凭据路径的实际值要掩码");
        assert_eq!(out[1].actual, "Bearer abcd***");
        assert_eq!(out[2].actual, "200", "status 不是凭据，原样");
        assert_eq!(out[3].actual, "2", "普通字段原样");
        // 占位符要放过：掩成 *** 会让"没这个字段"与"值被藏了"无从分辨
        assert_eq!(out[4].actual, "∅", "路径不存在的占位符不掩码");
        assert_eq!(out[0].expected, "—", "无期望值的占位符不掩码");

        // 非凭据路径上取到了别处回显的凭据 —— 仍按已知值做字面替换
        let out = redact_assertions(vec![rec("$.echo", "prefix-KNOWNSECRET-suffix", "x")], &["KNOWNSECRET".into()], true);
        assert!(!out[0].actual.contains("KNOWNSECRET"), "{}", out[0].actual);

        // 关掉脱敏就原样返回
        let raw = vec![rec("$.data.token", "PLAIN", "x")];
        assert_eq!(redact_assertions(raw.clone(), &[], false), raw);
    }

    #[test]
    fn secret_target_detection() {
        for t in ["$.data.token", "header.Authorization", "$.data['api-key']", "$.a.password", "token"] {
            assert!(is_secret_target(t), "{t} 应判为凭据路径");
        }
        for t in ["status", "$.args.page", "$.data.name", "header.Content-Type"] {
            assert!(!is_secret_target(t), "{t} 不该判为凭据路径");
        }
    }

    #[test]
    fn outputs_are_masked_in_report() {
        let outs: BTreeMap<String, Value> =
            [("token".to_string(), json!("abcdefgh")), ("id".to_string(), json!(42))].into_iter().collect();
        let r = redact_outputs(&outs, true);
        assert_eq!(r.get("token"), Some(&json!("abcd***")));
        assert_eq!(r.get("id"), Some(&json!(42)), "非凭据原样");
        assert_eq!(redact_outputs(&outs, false), outs);
    }
}
