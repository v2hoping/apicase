//! 给人看的输出。
//!
//! 两条原则：
//!
//! - **进度逐行打印，不做原地刷新**。CI 日志里被覆盖的行会变成一团乱码，
//!   而 CI 正是 CLI 最主要的用武之地。逐行还能 grep、能贴进 issue。
//! - **失败要带现场**。一行「✕ login.yml」只告诉你出事了，得再跑一次开着 -v 才知道
//!   哪条断言、期望什么、实际什么——而那时服务可能已经不是刚才那个状态了。

use crate::ops::check::{CheckReport, Severity};
use crate::ops::list::ListItem;
use crate::style::{pad, width, Style};
use apicase_core::report::{CaseResult, CaseStatus, EnvironmentInfo, RunReport, RunStatus, StepStatus};

/// 用例状态对应的记号。四种状态四个字形，灰度打印下也分得开
/// （只靠颜色区分的话，重定向到文件就全没了）。
pub fn case_mark(st: CaseStatus, s: &Style) -> String {
    match st {
        CaseStatus::Passed => s.pass("✓"),
        CaseStatus::Failed => s.fail("✕"),
        CaseStatus::Error => s.error("!"),
        CaseStatus::Skipped => s.skip("–"),
        CaseStatus::Running => s.dim("·"),
    }
}

/// 运行开头那两行：跑的是哪个工作空间、哪套环境、多少个用例。
///
/// 环境名要在开头就说清楚——一份「全绿」的输出如果跑的是错的那套环境，
/// 它比红色的失败更危险，而这件事只有在开跑前说才来得及叫停。
pub fn run_header(ws: &apicase_core::report::WorkspaceInfo, env: &EnvironmentInfo, total: usize, s: &Style) -> String {
    format!(
        "{} {} {}\n{} {}\n\n",
        s.dim("工作空间"),
        s.bold(&ws.name),
        s.dim(&format!("({})  · 环境 {} · {} 个变量", ws.root, env.name, env.vars.len())),
        s.dim("用例"),
        total
    )
}

/// 一个 case 跑完的那一行（外加失败时的现场）。
pub fn case_line(c: &CaseResult, s: &Style, name_col: usize) -> String {
    let dur = if c.status == CaseStatus::Skipped {
        "—".to_string()
    } else {
        format!("{}ms", c.duration_ms)
    };
    let mut line = format!("  {} {} {}", case_mark(c.status, s), pad(&c.file, name_col), s.dim(&pad(&dur, 8)));

    if let Some(r) = c.skip_reason.as_deref() {
        line.push_str(&s.skip(r));
    } else if let Some(note) = summary_note(c) {
        line.push_str(&note);
    }
    line.push('\n');

    // 失败现场：只列没过的断言与错误，通过的不占地方
    for st in &c.steps {
        if let Some(e) = st.error.as_deref() {
            line.push_str(&format!("      {} {}\n", s.error(&st.id), s.dim(e)));
        }
        if let Some(r) = st.skip_reason.as_deref().filter(|_| st.status == StepStatus::Skipped) {
            line.push_str(&format!("      {} {}\n", s.skip(&st.id), s.skip(r)));
        }
        for a in st.assertions.iter().filter(|a| !a.ok) {
            line.push_str(&format!(
                "      {} {} {} {} {} {}\n",
                s.dim(&st.id),
                a.target,
                s.dim(a.op.as_str()),
                a.expected,
                s.dim("→"),
                s.fail(&a.actual)
            ));
        }
    }
    line
}

/// 行尾那句话：失败时说断言几比几，错误时说是哪一步挂的。
fn summary_note(c: &CaseResult) -> Option<String> {
    match c.status {
        CaseStatus::Failed => {
            let total = c.steps.iter().map(|s| s.assertions.len()).sum::<usize>();
            let bad = c.steps.iter().flat_map(|s| &s.assertions).filter(|a| !a.ok).count();
            Some(format!("断言 {}/{total} 未通过", bad))
        }
        CaseStatus::Error => c
            .steps
            .iter()
            .find(|s| s.status == StepStatus::Error)
            .and_then(|s| s.error.clone()),
        _ => None,
    }
}

/// 列宽：最长的那个文件名，夹在 24~60 列之间。
///
/// 下限保证短名字也排得整齐，上限防止一个特别长的路径把整张表推到屏幕外
/// （超过上限的那行自己溢出，剩下的仍对齐）。
pub fn name_column(files: impl Iterator<Item = String>) -> usize {
    files.map(|f| width(&f)).max().unwrap_or(24).clamp(24, 60) + 2
}

/// 收尾的三行统计 + 耗时 + 报告位置。
///
/// **用例、请求、断言三个维度分开列**：用例级的「跳过」是这个文件没跑，
/// 请求级的是上游挂了没轮到它。合并计数会让「跳过 0 而报告里躺着两个灰点」
/// 这种自相矛盾的读数出现。
pub fn run_summary(report: &RunReport, report_path: Option<&std::path::Path>, s: &Style) -> String {
    let sm = &report.summary;
    let mut out = String::new();

    let counts = |label: &str, total: u32, passed: u32, failed: u32, error: u32, skipped: u32| {
        let mut line = format!("{} {}", s.dim(&pad(label, 4)), pad(&total.to_string(), 5));
        if passed > 0 {
            line.push_str(&format!("  {} {}", s.pass("通过"), pad(&passed.to_string(), 4)));
        }
        if failed > 0 {
            line.push_str(&format!("  {} {}", s.fail("失败"), pad(&failed.to_string(), 4)));
        }
        if error > 0 {
            line.push_str(&format!("  {} {}", s.error("错误"), pad(&error.to_string(), 4)));
        }
        if skipped > 0 {
            line.push_str(&format!("  {} {}", s.skip("跳过"), pad(&skipped.to_string(), 4)));
        }
        line.push('\n');
        line
    };

    out.push('\n');
    out.push_str(&counts("用例", sm.total, sm.passed, sm.failed, sm.error, sm.skipped));
    let t = sm.steps;
    if t.total > 0 {
        out.push_str(&counts("请求", t.total, t.passed, t.failed, t.error, t.skipped));
    }
    let a = sm.assertions;
    if a.total > 0 {
        out.push_str(&counts("断言", a.total, a.passed, a.failed, 0, 0));
    }

    out.push_str(&format!("{} {}\n", s.dim(pad("耗时", 4).as_str()), human_ms(report.duration_ms)));
    if let Some(p) = report_path {
        out.push_str(&format!("{} {}\n", s.dim(pad("报告", 4).as_str()), p.display()));
    }
    if report.status == RunStatus::Cancelled {
        out.push_str(&s.warn("运行被中断，以上是已完成的部分\n"));
    }
    out
}

/// 毫秒 → 人读的时长。超过一秒就用秒——「3218ms」要在脑子里除一次。
pub fn human_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// `apicase ls` 的表格。
pub fn list_table(items: &[ListItem], s: &Style) -> String {
    if items.is_empty() {
        return s.dim("没有找到用例\n").to_string();
    }
    let col = name_column(items.iter().map(|i| i.file.clone()));
    let mut out = String::new();
    for it in items {
        if !it.valid {
            out.push_str(&format!(
                "{} {}\n",
                s.fail(&pad(&it.file, col)),
                s.dim(it.error.as_deref().unwrap_or("解析失败"))
            ));
            continue;
        }
        let title = it.name.clone().unwrap_or_default();
        out.push_str(&format!("{}{}\n", pad(&it.file, col), s.dim(&title)));
        for st in &it.steps {
            let deps = if st.depends_on.is_empty() {
                String::new()
            } else {
                s.dim(&format!("  ← {}", st.depends_on.join(", ")))
            };
            out.push_str(&format!(
                "  {} {} {}{}\n",
                s.dim(&pad(&st.id, 16)),
                pad(&st.method, 6),
                st.url,
                deps
            ));
        }
    }
    out.push_str(&s.dim(&format!("\n{} 个用例\n", items.len())));
    out
}

/// `apicase check` 的结果。
///
/// **通过的只汇总一行**：校验一百个用例时，九十九行「✓」会把那一行真正要看的挤走。
pub fn check_text(report: &CheckReport, s: &Style) -> String {
    let mut out = String::new();
    let col = name_column(report.results.iter().map(|r| r.file.clone()));

    for r in report.results.iter().filter(|r| !r.issues.is_empty()) {
        let mark = if r.ok { s.warn("!") } else { s.fail("✕") };
        out.push_str(&format!("  {} {}\n", mark, pad(&r.file, col)));
        for i in &r.issues {
            let tag = match i.severity {
                Severity::Error => s.fail("错误"),
                Severity::Warning => s.warn("警告"),
            };
            let at = if i.at.is_empty() { String::new() } else { format!("{} ", s.dim(&i.at)) };
            out.push_str(&format!("      {tag} {at}{}\n", i.message));
        }
    }

    let clean = report.total - report.results.iter().filter(|r| !r.issues.is_empty()).count();
    out.push('\n');
    out.push_str(&format!("{} 个用例", report.total));
    if clean > 0 {
        out.push_str(&format!("，{} 无问题", s.pass(&clean.to_string())));
    }
    if report.errors > 0 {
        out.push_str(&format!("，{} 处错误", s.fail(&report.errors.to_string())));
    }
    if report.warnings > 0 {
        out.push_str(&format!("，{} 处警告", s.warn(&report.warnings.to_string())));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ColorWhen;
    use apicase_core::report::*;
    use std::collections::BTreeMap;

    fn plain() -> Style {
        Style::resolve(ColorWhen::Never, false)
    }

    fn case(status: CaseStatus, steps: Vec<StepResult>) -> CaseResult {
        CaseResult {
            file: "api/login.yml".into(),
            name: "登录".into(),
            status,
            skip_reason: None,
            started_at: "2026-08-03T00:00:00.000Z".into(),
            duration_ms: 318,
            steps,
        }
    }

    fn step(id: &str, status: StepStatus) -> StepResult {
        StepResult {
            id: id.into(),
            status,
            duration_ms: 12,
            request: None,
            response: None,
            outputs: BTreeMap::new(),
            assertions: Vec::new(),
            error: None,
            skip_reason: None,
        }
    }

    #[test]
    fn human_ms_switches_units() {
        assert_eq!(human_ms(42), "42ms");
        assert_eq!(human_ms(999), "999ms");
        assert_eq!(human_ms(3218), "3.2s");
        assert_eq!(human_ms(65_000), "1m5s");
    }

    /// 失败那行必须带上现场：哪条断言、期望什么、实际什么
    #[test]
    fn failed_case_line_carries_the_failing_assertion() {
        let mut st = step("login", StepStatus::Failed);
        st.assertions = vec![
            AssertRecord {
                target: "res.status".into(),
                op: "eq".into(),
                expected: "200".into(),
                actual: "200".into(),
                ok: true,
            },
            AssertRecord {
                target: "res.body.data.token".into(),
                op: "exists".into(),
                expected: "—".into(),
                actual: "∅".into(),
                ok: false,
            },
        ];
        let out = case_line(&case(CaseStatus::Failed, vec![st]), &plain(), 24);
        assert!(out.contains("✕"), "{out}");
        assert!(out.contains("断言 1/2 未通过"), "{out}");
        assert!(out.contains("res.body.data.token"), "失败的断言要列出来：{out}");
        assert!(!out.contains("res.status"), "通过的断言不该占地方：{out}");
    }

    /// error 与 failed 是两回事：前者要说清是哪一步、什么错
    #[test]
    fn error_case_line_shows_the_transport_error() {
        let mut st = step("login", StepStatus::Error);
        st.error = Some("连接被拒绝 (127.0.0.1:8080)".into());
        let out = case_line(&case(CaseStatus::Error, vec![st]), &plain(), 24);
        assert!(out.contains("!"), "{out}");
        assert!(out.contains("连接被拒绝 (127.0.0.1:8080)"), "{out}");
    }

    #[test]
    fn skipped_step_shows_its_root_cause() {
        let st = StepResult::skipped("createOrder", "上游 login 失败");
        let out = case_line(&case(CaseStatus::Failed, vec![st]), &plain(), 24);
        assert!(out.contains("上游 login 失败"), "{out}");
    }

    /// 三个维度分开列，且计数为 0 的那一档不占位置
    #[test]
    fn summary_lists_cases_steps_and_assertions_separately() {
        let mut r = crate::render::tests::sample_report();
        r.summary = RunSummary {
            total: 12,
            passed: 10,
            failed: 1,
            error: 1,
            skipped: 0,
            assertions: AssertSummary { total: 54, passed: 52, failed: 2 },
            steps: StepSummary { total: 28, passed: 25, failed: 1, error: 1, skipped: 1 },
        };
        let out = run_summary(&r, None, &plain());
        assert!(out.contains("用例"), "{out}");
        assert!(out.contains("请求"), "{out}");
        assert!(out.contains("断言"), "{out}");
        // 用例级 skipped 是 0，那一档不该出现在用例行里
        let case_row = out.lines().find(|l| l.starts_with("用例")).expect("应有用例行");
        assert!(!case_row.contains("跳过"), "计数为 0 的档不占位置：{case_row}");
        let step_row = out.lines().find(|l| l.starts_with("请求")).expect("应有请求行");
        assert!(step_row.contains("跳过"), "请求级跳过 1 要显示：{step_row}");
    }

    #[test]
    fn cancelled_run_says_so() {
        let mut r = sample_report();
        r.status = RunStatus::Cancelled;
        assert!(run_summary(&r, None, &plain()).contains("运行被中断"));
    }

    pub(super) fn sample_report() -> RunReport {
        RunReport {
            schema_version: REPORT_SCHEMA_VERSION,
            tool: ToolInfo { name: "apicase".into(), version: "0.1.0".into() },
            started_at: "2026-08-03T00:00:00.000Z".into(),
            finished_at: Some("2026-08-03T00:00:03.000Z".into()),
            duration_ms: 3218,
            status: RunStatus::Done,
            workspace: WorkspaceInfo { name: "demo".into(), root: "/demo".into() },
            environment: EnvironmentInfo { name: "dev".into(), vars: BTreeMap::new() },
            options: RunOptions {
                targets: vec![".".into()],
                recursive: true,
                environment: "dev".into(),
                concurrency: 1,
                stop_on_failure: false,
                max_body_bytes: 65536,
                continue_on_assertion_failure: false,
            },
            summary: RunSummary::default(),
            cases: Vec::new(),
        }
    }
}
