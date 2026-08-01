//! 运行报告渲染器：`RunReport` → 单文件自包含 HTML，以及反向的 `parse_report_html`。
//!
//! # 报告只有这一套渲染实现（硬约束）
//!
//! `REPORT_SHELL`（空壳 + 等宿主 postMessage 推数据）供 apicase 内嵌 iframe，
//! `render_html`（空壳 + 内联数据）供落盘与 CLI——**两者共用同一份 CSS 与渲染 JS**，
//! 因此「应用内看到的」「历史回看的」「发给同事的」三处像素级一致。
//! 若两边各写一套，改一次配色就要改两处，时间一长必然漂移；这不是"注意维护"
//! 能兜住的，是结构问题。
//!
//! # 为什么必须是单文件
//!
//! 诉求是「任意有浏览器的地方打开、不依赖 apicase」。多文件报告在 `file://` 下
//! fetch 同目录文件会被 CORS 拦掉（Playwright 正是栽在这，官方只能让你起本地服务器）。
//! 所以数据内联在 `<script type="application/json">` 里，不另存 `report.json`。
//!
//! # 资源在旁边的三个文件里
//!
//! `render/report.css`、`render/report.body.html`、`render/report.js` 由 `include_str!`
//! 编译进二进制。拆出来是因为它们是 CSS / HTML / JS——放在 Rust 字符串字面量里
//! 就再也没有语法高亮、没有格式化、没有搜索。

use crate::report::{RunReport, REPORT_SCHEMA_VERSION};

const CSS: &str = include_str!("render/report.css");
const BODY: &str = include_str!("render/report.body.html");
const JS: &str = include_str!("render/report.js");

/// 内联数据的锚点。`parse_report_html` 靠它切片——**改这里必须同步改 parse 侧**。
const DATA_OPEN: &str = r#"<script id="apicase-report" type="application/json">"#;
const DATA_CLOSE: &str = "</script>";

/// 内联 JSON 的转义：
///
/// - `</` → `<\/`：响应体里出现 `</script>` 会提前闭合 script 标签，整个报告就废了；
/// - U+2028 / U+2029：JSON 里合法，但在 JS 源码中是换行符，会破坏解析。
///
/// 三者的替换结果都仍是**合法 JSON**（`\/` 与 `\uXXXX` 都是标准转义），
/// 故 `parse_report_html` 直接 parse 即可，无需反向还原。
fn escape_json_for_script(json: &str) -> String {
    json.replace("</", r"<\/")
        .replace('\u{2028}', r"\u2028")
        .replace('\u{2029}', r"\u2029")
}

fn shell_html(data_json: &str) -> String {
    format!(
        "<!doctype html>\n\
<html lang=\"zh-CN\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"generator\" content=\"apicase\">\n\
<title>apicase 运行报告</title>\n\
<style>{CSS}</style>\n\
</head>\n\
<body>\n\
{BODY}{DATA_OPEN}{data_json}{DATA_CLOSE}\n\
<script>{JS}{DATA_CLOSE}\n\
</body>\n\
</html>\n"
    )
}

/// 报告空壳：无数据，等宿主 postMessage 推送。
/// apicase 内嵌 iframe 用它，与落盘报告共用同一套 CSS 与渲染 JS。
pub fn report_shell() -> String {
    shell_html("")
}

/// `RunReport` → 单文件自包含 HTML（落盘与 CLI 用）。
pub fn render_html(report: &RunReport) -> String {
    let json = serde_json::to_string(report).unwrap_or_else(|_| "null".into());
    shell_html(&escape_json_for_script(&json))
}

/// 反向：从报告 HTML 里切出内联的 `RunReport`。
///
/// 提取失败一律返回 `None`（普通 HTML、被手工编辑过的报告），调用方据此降级为
/// 纯文本视图——**能解析出结构化数据的才当报告**，等于隐含地做了一次格式校验。
pub fn parse_report_html(text: &str) -> Option<RunReport> {
    let start = text.find(DATA_OPEN)?;
    let from = start + DATA_OPEN.len();
    let end = text[from..].find(DATA_CLOSE)? + from;
    let raw = text[from..end].trim();
    if raw.is_empty() {
        return None;
    }
    let parsed: RunReport = serde_json::from_str(raw).ok()?;
    // schemaVersion 在读回时兑现用处：将来格式演进要在这里分流 / 迁移
    (parsed.schema_version == REPORT_SCHEMA_VERSION).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::*;
    use std::collections::BTreeMap;

    fn sample(body: &str) -> RunReport {
        RunReport {
            schema_version: REPORT_SCHEMA_VERSION,
            tool: ToolInfo { name: "apicase".into(), version: "0.1.0".into() },
            started_at: "2026-07-30T00:00:00.000Z".into(),
            finished_at: Some("2026-07-30T00:00:03.000Z".into()),
            duration_ms: 3000,
            status: RunStatus::Done,
            workspace: WorkspaceInfo { name: "演示".into(), root: "/w".into() },
            environment: EnvironmentInfo {
                name: "dev".into(),
                vars: [("baseUrl".to_string(), "http://x".to_string())].into_iter().collect(),
            },
            options: RunOptions {
                targets: vec!["01-登录".into()],
                recursive: true,
                environment: "dev".into(),
                concurrency: 1,
                stop_on_failure: false,
                redact: true,
                max_body_bytes: 65536,
            },
            summary: RunSummary { total: 1, passed: 1, ..Default::default() },
            cases: vec![CaseResult {
                file: "01-登录/a.yml".into(),
                name: "登录".into(),
                status: CaseStatus::Passed,
                skip_reason: None,
                started_at: "2026-07-30T00:00:00.000Z".into(),
                duration_ms: 12,
                steps: vec![StepResult {
                    id: "s1".into(),
                    status: StepStatus::Passed,
                    duration_ms: 12,
                    request: None,
                    response: Some(ResponseRecord {
                        status: 200,
                        status_text: "OK".into(),
                        headers: vec![KvPair::new("Content-Type", "application/json")],
                        body: BodyRecord { preview: Some(body.into()), bytes: body.len(), truncated: false },
                        elapsed_ms: 10,
                    }),
                    outputs: BTreeMap::new(),
                    assertions: vec![AssertRecord {
                        target: "res.status".into(),
                        op: "eq".into(),
                        expected: "200".into(),
                        actual: "200".into(),
                        ok: true,
                    }],
                    error: None,
                }],
            }],
        }
    }

    /// 往返等价是这条链路的护栏：改 `render_html` 时忘了同步 parse 侧，
    /// 历史报告会静默变成一堆 HTML 源码。
    #[test]
    fn render_then_parse_roundtrips() {
        let r = sample(r#"{"ok":true}"#);
        let html = render_html(&r);
        let back = parse_report_html(&html).expect("应能读回");
        assert_eq!(back, r);
    }

    /// 空壳与落盘报告**除数据块外逐字相同**——这是"只有一套渲染实现"的机器证明。
    #[test]
    fn shell_and_rendered_differ_only_in_the_data_block() {
        let shell = report_shell();
        let rendered = render_html(&sample("{}"));
        let cut = |s: &str| {
            let a = s.find(DATA_OPEN).expect("应有数据块") + DATA_OPEN.len();
            let b = s[a..].find(DATA_CLOSE).expect("应有闭合") + a;
            (s[..a].to_string(), s[b..].to_string())
        };
        assert_eq!(cut(&shell), cut(&rendered));
        // 空壳里的数据块确实是空的（等宿主推送）
        assert!(shell.contains(&format!("{DATA_OPEN}{DATA_CLOSE}")));
    }

    /// 响应体里出现 `</script>` 会提前闭合标签，把整个报告打废
    #[test]
    fn script_tags_in_body_cannot_break_out() {
        let evil = r#"{"html":"</script><script>alert(1)</script>"}"#;
        let html = render_html(&sample(evil));
        let data_start = html.find(DATA_OPEN).unwrap() + DATA_OPEN.len();
        let data_end = html[data_start..].find(DATA_CLOSE).unwrap() + data_start;
        let block = &html[data_start..data_end];
        assert!(!block.contains("</script"), "数据块内不该出现未转义的闭合标签");
        assert!(block.contains(r"<\/script"), "应被转义成 <\\/script");
        // 转义后仍是合法 JSON，能原样读回
        let back = parse_report_html(&html).expect("应能读回");
        assert_eq!(
            back.cases[0].steps[0].response.as_ref().unwrap().body.preview.as_deref(),
            Some(evil)
        );
    }

    /// U+2028 / U+2029 在 JSON 里合法，但在 JS 源码里是换行符
    #[test]
    fn line_separators_are_escaped() {
        let r = sample("a\u{2028}b\u{2029}c");
        let html = render_html(&r);
        assert!(!html.contains('\u{2028}'), "U+2028 必须被转义");
        assert!(!html.contains('\u{2029}'), "U+2029 必须被转义");
        assert_eq!(parse_report_html(&html).unwrap(), r);
    }

    /// 只认自己生成的报告：普通 HTML、空数据块、坏 JSON、版本不符一律降级
    #[test]
    fn only_recognizes_its_own_reports() {
        assert!(parse_report_html("").is_none());
        assert!(parse_report_html("<html><body>普通网页</body></html>").is_none());
        assert!(parse_report_html(&report_shell()).is_none(), "空壳没有数据");
        assert!(parse_report_html(&format!("{DATA_OPEN}不是 JSON{DATA_CLOSE}")).is_none());

        // schemaVersion 不匹配 → 拒绝（将来格式演进的分流点）
        let mut r = sample("{}");
        r.schema_version = 999;
        let json = serde_json::to_string(&r).unwrap();
        assert!(parse_report_html(&format!("{DATA_OPEN}{json}{DATA_CLOSE}")).is_none());
    }

    /// 报告要能脱离 apicase 打开：不引用任何外部资源
    #[test]
    fn report_is_fully_self_contained() {
        let html = render_html(&sample("{}"));
        for marker in ["<link ", "src=\"http", "src='http", "@import", "url(http"] {
            assert!(!html.contains(marker), "报告不该引用外部资源：{marker}");
        }
        assert!(html.contains("<style>"), "样式内联");
        assert!(html.starts_with("<!doctype html>"), "是一份完整文档");
        assert!(html.ends_with("</html>\n"));
    }
}
