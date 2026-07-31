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

/// `status` / `header.<名>` / JSONPath 三种取法。
fn actual_for(target: &str, resp: &RespView, body: Option<&Value>) -> Option<Value> {
    if target == "status" {
        return Some(Value::from(resp.status));
    }
    if target.len() > 7 && target[..7].eq_ignore_ascii_case("header.") {
        let want = target[7..].to_ascii_lowercase();
        return resp
            .headers
            .iter()
            .find(|h| h.key.to_ascii_lowercase() == want)
            .map(|h| Value::String(h.value.clone()));
    }
    jsonpath::get(body?, target).cloned()
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
        assert!(one("status", AssertOp::Eq, Some("200"), BODY).ok);
        assert!(!one("status", AssertOp::Eq, Some("404"), BODY).ok);
        assert!(one("status", AssertOp::Lt, Some("300"), BODY).ok);
        assert!(one("status", AssertOp::Gt, Some("199"), BODY).ok);
    }

    #[test]
    fn header_target_is_case_insensitive() {
        assert!(one("header.content-type", AssertOp::Contains, Some("json"), BODY).ok);
        assert!(one("Header.Content-Type", AssertOp::Contains, Some("json"), BODY).ok);
        assert!(one("header.X-Count", AssertOp::Eq, Some("7"), BODY).ok);
        let miss = one("header.nope", AssertOp::Exists, None, BODY);
        assert!(!miss.ok);
        assert_eq!(miss.actual, "∅");
    }

    #[test]
    fn jsonpath_target() {
        assert!(one("$.data.token", AssertOp::Eq, Some("abcdef"), BODY).ok);
        assert!(one("$.data.count", AssertOp::Eq, Some("7"), BODY).ok);
        assert!(one("$.list[2]", AssertOp::Eq, Some("3"), BODY).ok);
        assert!(one("$.data.flag", AssertOp::Eq, Some("true"), BODY).ok);
    }

    /// 数字 200 与字符串 "200" 都该等于期望值 200
    #[test]
    fn comparison_is_loose_across_types() {
        assert!(one("$.data.count", AssertOp::Eq, Some("7"), BODY).ok, "数字 vs 文本期望");
        assert!(one("$.code", AssertOp::Eq, Some("0"), BODY).ok);
        let s = r#"{"n":"7"}"#;
        assert!(one("$.n", AssertOp::Eq, Some("7"), s).ok, "字符串 vs 文本期望");
        assert!(one("$.n", AssertOp::Gt, Some("6"), s).ok, "字符串也参与数值比较");
    }

    /// 取到 null 与路径不存在都算"不存在"，但展示要能区分
    #[test]
    fn exists_treats_null_as_absent() {
        assert!(!one("$.data.nil", AssertOp::Exists, None, BODY).ok);
        assert!(one("$.data.nil", AssertOp::NotExists, None, BODY).ok);
        assert!(one("$.data.token", AssertOp::Exists, None, BODY).ok);
        assert!(one("$.nope", AssertOp::NotExists, None, BODY).ok);
        assert_eq!(one("$.data.nil", AssertOp::Exists, None, BODY).actual, "null", "取到 null 显示 null");
        assert_eq!(one("$.nope", AssertOp::Exists, None, BODY).actual, "∅", "路径不存在显示 ∅");
    }

    /// exists / notExists 的期望值列用 — 占位
    #[test]
    fn valueless_ops_show_dash() {
        assert_eq!(one("$.a", AssertOp::Exists, Some("忽略"), BODY).expected, "—");
        assert_eq!(one("status", AssertOp::Eq, Some("200"), BODY).expected, "200");
    }

    #[test]
    fn contains_and_matches() {
        assert!(one("$.msg", AssertOp::Contains, Some("o"), BODY).ok);
        assert!(!one("$.msg", AssertOp::Contains, Some("zzz"), BODY).ok);
        assert!(one("$.data.token", AssertOp::Matches, Some("^abc"), BODY).ok);
        assert!(!one("$.data.token", AssertOp::Matches, Some("^xyz"), BODY).ok);
        // 正则写错 → false，而不是 panic
        assert!(!one("$.data.token", AssertOp::Matches, Some("[unclosed"), BODY).ok);
    }

    /// 非数字参与 gt/lt 恒 false（JS 里的 NaN 语义）
    #[test]
    fn non_numeric_comparisons_are_false() {
        assert!(!one("$.msg", AssertOp::Gt, Some("1"), BODY).ok);
        assert!(!one("$.msg", AssertOp::Lt, Some("1"), BODY).ok);
        assert!(!one("$.data.count", AssertOp::Gt, Some("非数字"), BODY).ok);
    }

    /// 响应体不是 JSON 时，JSONPath 目标一律取不到——但不该崩
    #[test]
    fn non_json_body_is_handled() {
        let r = one("$.a", AssertOp::Exists, None, "<html>not json</html>");
        assert!(!r.ok);
        assert_eq!(r.actual, "∅");
        // status 与 header 不依赖响应体，仍然可断言
        assert!(one("status", AssertOp::Eq, Some("200"), "<html>").ok);
    }

    /// 空 target 的断言（编辑器里的占位行）直接跳过，不产生结果行
    #[test]
    fn blank_targets_are_skipped() {
        let hs = headers();
        let list = vec![
            Assertion { target: "  ".into(), op: AssertOp::Eq, value: Some("1".into()) },
            Assertion { target: "status".into(), op: AssertOp::Eq, value: Some("200".into()) },
        ];
        let out = eval_assertions(&list, &resp(BODY, &hs));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].target, "status");
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
