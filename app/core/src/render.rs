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
    // **旧报告要读得回来**：v1 报告里只是没有 skipped 而已，当前渲染器完全能显示它。
    // 反过来不行——未来版本的报告可能带本渲染器不认识的状态或字段，宁可降级为纯文本
    // 也好过静默渲染错。所以是 `<=` 而不是 `==`。
    (parsed.schema_version <= REPORT_SCHEMA_VERSION).then_some(parsed)
}

/// 运行期间把报告周期性地写到文件：用户中途用浏览器打开刷新就能看到部分结果，
/// 进程被杀也留得下已跑完的部分。
///
/// 仍是**整份重渲染**（不做增量），但**间隔随报告增大而拉长**（见 `interval`）：
/// 小报告几毫秒的字符串拼接不值得为它做增量，大报告则不该每秒把已完成的几十 MB 重渲一遍。
/// 应用内 / 终端里的进度另有实时通道，落盘只是归档与崩溃兜底。
///
/// **写盘要串行**——两次写交错会让报告文件出现半截内容，故持锁写。
/// 桌面壳与 CLI 共用这一份：两边各写一个必然在节流策略上分叉。
#[derive(Clone)]
pub struct ReportWriter {
    path: String,
    last: std::sync::Arc<std::sync::Mutex<std::time::Instant>>,
}

impl ReportWriter {
    /// 建一个写入器。**不建目录**——建目录会失败、而失败该由调用方在开跑前就报出来，
    /// 不是等到第一次落盘时静默吞掉。
    pub fn new(path: impl Into<String>) -> Self {
        // 初始时间往前推，保证第一次回调就会落盘
        let past = std::time::Instant::now() - std::time::Duration::from_secs(60);
        Self { path: path.into(), last: std::sync::Arc::new(std::sync::Mutex::new(past)) }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// 写盘间隔随报告增大而拉长：每次落盘都要把**整份**报告渲染成 HTML，
    /// 500 个用例可达几十 MB——固定 1 秒一次会让后半程一直在重复渲染同一堆已完成的数据。
    fn interval(cases: usize) -> std::time::Duration {
        std::time::Duration::from_millis(1000.max(cases as u64 * 20))
    }

    /// 距上次落盘够久才写。挂在进度回调里用。
    pub fn maybe_write(&self, r: &RunReport) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed() < Self::interval(r.cases.len()) {
            return;
        }
        *last = std::time::Instant::now();
        self.write_locked(r);
    }

    /// 无视节流立刻写。收尾时用——最后那份必须落下去。
    pub fn write_now(&self, r: &RunReport) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        *last = std::time::Instant::now();
        self.write_locked(r);
    }

    /// 写盘失败不中断运行——结果仍在应用内 / 终端里可见，报告只是少了一份归档。
    fn write_locked(&self, r: &RunReport) {
        let _ = std::fs::write(&self.path, render_html(r));
    }
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
                max_body_bytes: 65536,
                continue_on_assertion_failure: false,
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
                    skip_reason: None,
                }],
            }],
        }
    }

    /// 写盘节流：小报告保持 1 秒一次（崩溃兜底），大报告拉长——
    /// 每次落盘都要渲染整份 HTML，后半程重复渲染的全是已完成的数据。
    #[test]
    fn report_write_interval_grows_with_report_size() {
        use std::time::Duration;
        assert_eq!(ReportWriter::interval(0), Duration::from_millis(1000));
        assert_eq!(ReportWriter::interval(10), Duration::from_millis(1000), "小报告不受影响");
        assert_eq!(ReportWriter::interval(50), Duration::from_millis(1000), "临界点仍是 1 秒");
        assert_eq!(ReportWriter::interval(100), Duration::from_millis(2000));
        assert_eq!(ReportWriter::interval(500), Duration::from_millis(10_000));
    }

    /// 第一次回调就落盘（用户中途就能用浏览器打开看），随后 1s 内不重复写
    #[test]
    fn report_writer_throttles_after_the_first_write() {
        let path = std::env::temp_dir().join("apicase-writer-test.html");
        let _ = std::fs::remove_file(&path);
        let w = ReportWriter::new(path.to_string_lossy().into_owned());
        let r = sample("{}");

        w.maybe_write(&r);
        assert!(path.is_file(), "第一次回调就该落盘");
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();

        w.maybe_write(&r); // 1s 内的第二次应被节流掉
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first);

        w.write_now(&r); // 收尾那次强制写
        assert!(std::fs::read_to_string(&path).unwrap().contains("apicase-report"));
        let _ = std::fs::remove_file(&path);
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

        // 比自己新的 schemaVersion → 拒绝（可能带本渲染器不认识的状态，宁可降级为纯文本）
        let mut r = sample("{}");
        r.schema_version = REPORT_SCHEMA_VERSION + 1;
        let json = serde_json::to_string(&r).unwrap();
        assert!(parse_report_html(&format!("{DATA_OPEN}{json}{DATA_CLOSE}")).is_none());
    }

    /// **历史报告必须仍打得开**：v1 里只是没有 skipped 而已，当前渲染器完全能显示。
    /// 这条是 v1 → v2 那次 bump 留下的护栏——版本号涨了就把老报告全锁死，是回归。
    #[test]
    fn older_reports_still_parse() {
        let mut r = sample("{}");
        r.schema_version = 1;
        let json = serde_json::to_string(&r).unwrap();
        let back = parse_report_html(&format!("{DATA_OPEN}{json}{DATA_CLOSE}"));
        assert_eq!(back.map(|p| p.schema_version), Some(1), "v1 报告要读得回来");
    }

    /// 跳过的 step 要显示原因，且不显示会误导人的 "0ms"
    #[test]
    fn skipped_step_shows_its_reason() {
        let mut r = sample("{}");
        r.cases[0].steps.push(StepResult::skipped("下游", "上游 s1 失败"));
        let html = render_html(&r);
        assert!(html.contains("上游 s1 失败"), "跳过原因要进报告");
        // 数据是内联的，渲染在浏览器里做——这里守住数据契约即可
        let back = parse_report_html(&html).expect("读得回来");
        let last = back.cases[0].steps.last().unwrap();
        assert_eq!(last.status, StepStatus::Skipped);
        assert_eq!(last.skip_reason.as_deref(), Some("上游 s1 失败"));
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
