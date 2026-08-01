//! 断言评估与输出提取 —— 一个 step 跑完之后对响应做的全部判断。
//!
//! 比较是**宽松的**（`status eq 200` 同时匹配数字 200 与字符串 `"200"`）。
//! 这不是偷懒：断言里的期望值来自 YAML，YAML 里 `value: 200` 与 `value: '200'`
//! 都写得出来，而 JSON 响应里 `"status": "200"` 也很常见。严格比较会让用户
//! 在"为什么明明一样却不通过"上耗掉大量时间，收益却只是理论上的严谨。

use crate::jsonpath;
use crate::model::{AssertOp, Assertion, StepOutput};
use crate::report::{AssertRecord, KvPair};
use crate::util::{js_number, js_number_str, js_string};
use serde_json::Value;
use std::collections::BTreeMap;

/// 断言要看的响应切面（不绑定具体的响应类型，便于测试与复用）。
pub struct RespView<'a> {
    pub status: u16,
    pub headers: &'a [KvPair],
    pub body: &'a str,
}

/// 从响应体按 `outputs` 提取变量。
/// 提取不到记 `null`——**键要在**，否则下游 `{{steps.x.outputs.y}}` 会因为
/// "变量不存在"而保留字面量，看起来像是没配，实际是上游没返回。
pub fn extract_outputs(outputs: &[StepOutput], body: &str) -> BTreeMap<String, Value> {
    let parsed = jsonpath::parse_json(body);
    let mut out = BTreeMap::new();
    for o in outputs {
        let name = o.name.trim();
        if name.is_empty() {
            continue;
        }
        let v = parsed
            .as_ref()
            .and_then(|root| jsonpath::get(root, &o.path))
            .cloned()
            .unwrap_or(Value::Null);
        out.insert(name.to_string(), v);
    }
    out
}

/// 评估一组断言，返回逐条结果（顺序与配置一致，便于在报告里对照）。
pub fn eval_assertions(list: &[Assertion], resp: &RespView) -> Vec<AssertRecord> {
    let parsed = jsonpath::parse_json(resp.body);
    list.iter()
        .filter(|a| !a.target.trim().is_empty())
        .map(|a| {
            let actual = actual_for(a.target.trim(), resp, parsed.as_ref());
            let expected = a.value.clone().unwrap_or_default();
            AssertRecord {
                target: a.target.trim().to_string(),
                op: a.op.as_str().to_string(),
                expected: if a.op.needs_value() { expected.clone() } else { "—".into() },
                // ∅ 是"路径不存在"，与取到 null 值区分开——排查时这两者完全不同
                actual: actual.as_ref().map(js_string).unwrap_or_else(|| "∅".into()),
                ok: compare(actual.as_ref(), a.op, &expected),
            }
        })
        .collect()
}

/// 断言目标统一挂在 `res` 命名空间下：`res.status` / `res.headers.<名>` / `res.body<路径>`。
///
/// **只认这一种写法**——旧的 `status` / `header.X` / `$.data.token` 判为无效目标（取不到值）。
/// 统一前缀是为了对齐同类工具的直觉（"响应的东西都挂在 res 下"），而不是让用户记住
/// "status 裸写、header 加前缀、body 用 $" 三套规则。
fn actual_for(target: &str, resp: &RespView, body: Option<&Value>) -> Option<Value> {
    // `res` 之后必须是 `.`：`res` 单独、`response.x` 都不是目标
    let rest = target.trim().strip_prefix("res")?.strip_prefix('.')?;
    if rest == "status" {
        return Some(Value::from(resp.status));
    }
    if let Some(after) = domain_rest(rest, "headers") {
        let want = header_name(after)?.to_ascii_lowercase();
        return resp
            .headers
            .iter()
            .find(|h| h.key.to_ascii_lowercase() == want)
            .map(|h| Value::String(h.value.clone()));
    }
    if let Some(after) = domain_rest(rest, "body") {
        let Some(root) = body else {
            // 响应体不是 JSON：`res.body` 仍给原文——HTML / 纯文本做 contains 是真实需求；
            // 而 `res.body.x` 在这种响应上确实取不到，照旧算不存在。
            return after.is_empty().then(|| Value::String(resp.body.to_string()));
        };
        return jsonpath::get(root, after).cloned();
    }
    None
}

/// 取 `res.` 之后某个域（`headers` / `body`）的剩余部分。
/// 域名后必须是路径边界（`.` / `[`）或直接结束——`res.bodyfoo` 不算 body 域，
/// 否则拼错的目标会被当成"取整个响应体"而静默通过。
fn domain_rest<'a>(rest: &'a str, domain: &str) -> Option<&'a str> {
    let after = rest.strip_prefix(domain)?;
    (after.is_empty() || after.starts_with('.') || after.starts_with('[')).then_some(after)
}

/// `res.headers` 之后的头名：`.Content-Type` 或 `['Content-Type']` / `["Content-Type"]`。
/// 点号形式把剩余整段当名字，不再按点切分——HTTP 头名里没有嵌套结构，
/// 切了反而让 `res.headers.X.Y` 这种笔误变成"取不到的多层路径"而非"名字不存在"。
fn header_name(after: &str) -> Option<&str> {
    if let Some(name) = after.strip_prefix('.') {
        let name = name.trim();
        return (!name.is_empty()).then_some(name);
    }
    let inner = after.strip_prefix('[')?.strip_suffix(']')?;
    let name = inner
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')))?;
    (!name.is_empty()).then_some(name)
}

fn compare(actual: Option<&Value>, op: AssertOp, value: &str) -> bool {
    match op {
        // 只有"路径不存在"与"取到 null"才算不存在
        AssertOp::Exists => matches!(actual, Some(v) if !v.is_null()),
        AssertOp::NotExists => !matches!(actual, Some(v) if !v.is_null()),
        AssertOp::Eq => loose_eq(actual, value),
        AssertOp::Ne => !loose_eq(actual, value),
        AssertOp::Contains => actual
            .filter(|v| !v.is_null())
            .map(|v| js_string(v).contains(value))
            .unwrap_or(false),
        AssertOp::Gt => num_cmp(actual, value, |a, b| a > b),
        AssertOp::Lt => num_cmp(actual, value, |a, b| a < b),
        AssertOp::Matches => actual
            .map(|v| {
                // 正则写错就是 false（同前端行为）。注意 Rust 的 regex 不支持
                // 环视与反向引用——用到那些的表达式会被判为无效而非静默乱匹配。
                regex::Regex::new(value).map(|re| re.is_match(&js_string(v))).unwrap_or(false)
            })
            .unwrap_or(false),
    }
}

/// 先按文本比，再按数字比——`200`（数字）与 `"200"`（字符串）都该等于期望值 `200`。
fn loose_eq(actual: Option<&Value>, value: &str) -> bool {
    let Some(a) = actual.filter(|v| !v.is_null()) else {
        return false;
    };
    if js_string(a) == value {
        return true;
    }
    match (js_number(a), js_number_str(value)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

fn num_cmp(actual: Option<&Value>, value: &str, f: impl Fn(f64, f64) -> bool) -> bool {
    match (actual.and_then(js_number), js_number_str(value)) {
        // 任一侧算不出数字（JS 里的 NaN）参与比较恒 false
        (Some(a), Some(b)) => f(a, b),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers() -> Vec<KvPair> {
        vec![KvPair::new("Content-Type", "application/json"), KvPair::new("X-Count", "7")]
    }

    fn resp<'a>(body: &'a str, hs: &'a [KvPair]) -> RespView<'a> {
        RespView { status: 200, headers: hs, body }
    }

    fn one(target: &str, op: AssertOp, value: Option<&str>, body: &str) -> AssertRecord {
        let hs = headers();
        let a = Assertion { target: target.into(), op, value: value.map(str::to_string) };
        eval_assertions(std::slice::from_ref(&a), &resp(body, &hs)).remove(0)
    }

    const BODY: &str = r#"{"code":0,"msg":"ok","data":{"token":"abcdef","count":7,"flag":true,"nil":null},"list":[1,2,3]}"#;

    #[test]
    fn status_target() {
        assert!(one("res.status", AssertOp::Eq, Some("200"), BODY).ok);
        assert!(!one("res.status", AssertOp::Eq, Some("404"), BODY).ok);
        assert!(one("res.status", AssertOp::Lt, Some("300"), BODY).ok);
        assert!(one(" res.status ", AssertOp::Gt, Some("199"), BODY).ok, "两端空白应忽略");
    }

    #[test]
    fn header_target_is_case_insensitive() {
        assert!(one("res.headers.content-type", AssertOp::Contains, Some("json"), BODY).ok);
        assert!(one("res.headers.Content-Type", AssertOp::Contains, Some("json"), BODY).ok);
        assert!(one("res.headers.X-Count", AssertOp::Eq, Some("7"), BODY).ok);
        let miss = one("res.headers.nope", AssertOp::Exists, None, BODY);
        assert!(!miss.ok);
        assert_eq!(miss.actual, "∅");
    }

    /// 头名含点 / 空格时走方括号形式
    #[test]
    fn header_target_bracket_form() {
        assert!(one("res.headers['content-type']", AssertOp::Contains, Some("json"), BODY).ok);
        assert!(one("res.headers[\"X-Count\"]", AssertOp::Eq, Some("7"), BODY).ok);
        assert_eq!(one("res.headers['nope']", AssertOp::Exists, None, BODY).actual, "∅");
    }

    #[test]
    fn body_target() {
        assert!(one("res.body.data.token", AssertOp::Eq, Some("abcdef"), BODY).ok);
        assert!(one("res.body.data.count", AssertOp::Eq, Some("7"), BODY).ok);
        assert!(one("res.body.list[2]", AssertOp::Eq, Some("3"), BODY).ok);
        assert!(one("res.body.data.flag", AssertOp::Eq, Some("true"), BODY).ok);
        assert!(one("res.body[\"data\"].token", AssertOp::Eq, Some("abcdef"), BODY).ok);
    }

    /// JSON 的 key 带连字符很常见，点号形式要能直接写
    #[test]
    fn body_key_with_hyphen() {
        let b = r#"{"user-name":"张三","x":{"a-b-c":1}}"#;
        assert!(one("res.body.user-name", AssertOp::Eq, Some("张三"), b).ok);
        assert!(one("res.body.x.a-b-c", AssertOp::Eq, Some("1"), b).ok);
    }

    /// 旧写法（status / header.X / $.data.token）一律判无效——硬切换，不做双认
    #[test]
    fn legacy_targets_are_invalid() {
        for t in ["status", "header.Content-Type", "$.data.token", "$", "data.token"] {
            assert_eq!(one(t, AssertOp::Exists, None, BODY).actual, "∅", "{t} 应判为无效目标");
        }
    }

    /// 域名必须落在路径边界上，拼错的目标要显式失败而不是撞上别的域
    #[test]
    fn malformed_targets_are_invalid() {
        for t in ["res", "res.", "resbody", "response.status", "res.bodyfoo", "res.statusx", "res.headers", "res.headers.", "res.foo"] {
            assert_eq!(one(t, AssertOp::Exists, None, BODY).actual, "∅", "{t} 应判为无效目标");
        }
    }

    /// 整个响应体：JSON 给结构，非 JSON 给原文（HTML / 纯文本做 contains 是真实需求）
    #[test]
    fn whole_body_target() {
        assert!(one("res.body", AssertOp::Contains, Some("abcdef"), BODY).ok);
        assert!(one("res.body", AssertOp::Exists, None, BODY).ok);
        let html = "<html>hello</html>";
        assert!(one("res.body", AssertOp::Contains, Some("hello"), html).ok);
        assert!(one("res.body", AssertOp::Matches, Some("^<html>"), html).ok);
    }

    /// 数字 200 与字符串 "200" 都该等于期望值 200
    #[test]
    fn comparison_is_loose_across_types() {
        assert!(one("res.body.data.count", AssertOp::Eq, Some("7"), BODY).ok, "数字 vs 文本期望");
        assert!(one("res.body.code", AssertOp::Eq, Some("0"), BODY).ok);
        let s = r#"{"n":"7"}"#;
        assert!(one("res.body.n", AssertOp::Eq, Some("7"), s).ok, "字符串 vs 文本期望");
        assert!(one("res.body.n", AssertOp::Gt, Some("6"), s).ok, "字符串也参与数值比较");
    }

    /// 取到 null 与路径不存在都算"不存在"，但展示要能区分
    #[test]
    fn exists_treats_null_as_absent() {
        assert!(!one("res.body.data.nil", AssertOp::Exists, None, BODY).ok);
        assert!(one("res.body.data.nil", AssertOp::NotExists, None, BODY).ok);
        assert!(one("res.body.data.token", AssertOp::Exists, None, BODY).ok);
        assert!(one("res.body.nope", AssertOp::NotExists, None, BODY).ok);
        assert_eq!(one("res.body.data.nil", AssertOp::Exists, None, BODY).actual, "null", "取到 null 显示 null");
        assert_eq!(one("res.body.nope", AssertOp::Exists, None, BODY).actual, "∅", "路径不存在显示 ∅");
    }

    /// exists / notExists 的期望值列用 — 占位
    #[test]
    fn valueless_ops_show_dash() {
        assert_eq!(one("res.body.a", AssertOp::Exists, Some("忽略"), BODY).expected, "—");
        assert_eq!(one("res.status", AssertOp::Eq, Some("200"), BODY).expected, "200");
    }

    #[test]
    fn contains_and_matches() {
        assert!(one("res.body.msg", AssertOp::Contains, Some("o"), BODY).ok);
        assert!(!one("res.body.msg", AssertOp::Contains, Some("zzz"), BODY).ok);
        assert!(one("res.body.data.token", AssertOp::Matches, Some("^abc"), BODY).ok);
        assert!(!one("res.body.data.token", AssertOp::Matches, Some("^xyz"), BODY).ok);
        // 正则写错 → false，而不是 panic
        assert!(!one("res.body.data.token", AssertOp::Matches, Some("[unclosed"), BODY).ok);
    }

    /// 非数字参与 gt/lt 恒 false（JS 里的 NaN 语义）
    #[test]
    fn non_numeric_comparisons_are_false() {
        assert!(!one("res.body.msg", AssertOp::Gt, Some("1"), BODY).ok);
        assert!(!one("res.body.msg", AssertOp::Lt, Some("1"), BODY).ok);
        assert!(!one("res.body.data.count", AssertOp::Gt, Some("非数字"), BODY).ok);
    }

    /// 响应体不是 JSON 时，JSONPath 目标一律取不到——但不该崩
    #[test]
    fn non_json_body_is_handled() {
        let r = one("res.body.a", AssertOp::Exists, None, "<html>not json</html>");
        assert!(!r.ok);
        assert_eq!(r.actual, "∅");
        // status 与 header 不依赖响应体，仍然可断言
        assert!(one("res.status", AssertOp::Eq, Some("200"), "<html>").ok);
    }

    /// 空 target 的断言（编辑器里的占位行）直接跳过，不产生结果行
    #[test]
    fn blank_targets_are_skipped() {
        let hs = headers();
        let list = vec![
            Assertion { target: "  ".into(), op: AssertOp::Eq, value: Some("1".into()) },
            Assertion { target: "res.status".into(), op: AssertOp::Eq, value: Some("200".into()) },
        ];
        let out = eval_assertions(&list, &resp(BODY, &hs));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].target, "res.status");
    }

    #[test]
    fn extracts_outputs() {
        let outs = vec![
            StepOutput { name: "token".into(), path: "$.data.token".into() },
            StepOutput { name: "n".into(), path: "$.data.count".into() },
            StepOutput { name: "missing".into(), path: "$.nope".into() },
            StepOutput { name: "  ".into(), path: "$.code".into() },
        ];
        let got = extract_outputs(&outs, BODY);
        assert_eq!(got.get("token"), Some(&json!("abcdef")));
        assert_eq!(got.get("n"), Some(&json!(7)));
        // 提取不到也要有键（值为 null），否则下游看起来像"没配变量"
        assert_eq!(got.get("missing"), Some(&Value::Null));
        assert_eq!(got.len(), 3, "空名字的输出被跳过");
    }

    #[test]
    fn extract_outputs_on_non_json_body() {
        let outs = vec![StepOutput { name: "t".into(), path: "$.a".into() }];
        assert_eq!(extract_outputs(&outs, "plain text").get("t"), Some(&Value::Null));
    }
}
