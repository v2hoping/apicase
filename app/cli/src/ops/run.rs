//! 运行用例：从「一组路径 + 一堆选项」到一份 `RunReport`。

use super::{read_text, tool_version};
use apicase_core::discover;
use apicase_core::http::ProxyConfig;
use apicase_core::render::ReportWriter;
use apicase_core::report::{CaseResult, RunOptions, RunReport, RunStatus};
use apicase_core::runner::{self, BatchMeta, BatchTarget, Cancel, RunOpts};
use apicase_core::workspace::{self, Workspace};
use apicase_core::yaml;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// HTML 报告落在哪。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportSink {
    /// `<工作空间>/.apicase/reports/<时间戳>-<目标名>.html`（同桌面端，两边的历史因此在一起）
    Auto,
    /// 指定路径
    Path(PathBuf),
    /// 不落盘
    None,
}

/// 一次运行的全部输入。
///
/// 字段与 `apicase run` 的选项、`apicase_run` 工具的参数一一对应——三处同名同义，
/// 是这一层存在的意义。
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// 目标路径（文件或目录）。**空 = 整个工作空间**。
    pub targets: Vec<PathBuf>,
    /// 直接给一段 case YAML（`apicase run -` 从 stdin 读，MCP 的 `content` 参数）。
    /// 给了就忽略 `targets`——AI 生成一段用例直接跑，不必先落盘。
    pub content: Option<String>,
    /// 环境名。`None` = 工作空间的缺省环境。
    pub env: Option<String>,
    /// 只跑这些 step（**自动带上它们的上游依赖**）。空 = 全跑。
    pub steps: Vec<String>,
    /// 追加 / 覆盖环境变量。
    pub vars: Vec<(String, String)>,
    /// 用例之间的并发数。`None` = 跟随工作空间设置。
    pub concurrency: Option<u32>,
    pub stop_on_failure: bool,
    /// `None` = 跟随工作空间设置。
    pub continue_on_assertion_failure: Option<bool>,
    pub timeout_ms: Option<u64>,
    /// 跳过 TLS 证书校验。
    pub insecure: bool,
    pub recursive: bool,
    pub report: ReportSink,
    pub proxy: Option<ProxyConfig>,
}

impl Default for RunRequest {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            content: None,
            env: None,
            steps: Vec::new(),
            vars: Vec::new(),
            concurrency: None,
            stop_on_failure: false,
            continue_on_assertion_failure: None,
            timeout_ms: None,
            insecure: false,
            recursive: true,
            report: ReportSink::None,
            proxy: None,
        }
    }
}

/// 运行结果：报告本体 + 报告落在哪（没落盘就是 `None`）。
pub struct RunOutcome {
    pub report: RunReport,
    pub report_path: Option<PathBuf>,
}

impl RunOutcome {
    /// 退出码语义的判定源（外壳照此映射）。见 `crate::exit`。
    pub fn has_failures(&self) -> bool {
        self.report.summary.failed > 0
    }

    pub fn has_errors(&self) -> bool {
        let s = &self.report.summary;
        s.error > 0 || s.skipped > 0
    }
}

/// 内联 YAML 在报告里的文件名。用尖括号是为了一眼看出它不是磁盘上的路径。
pub const STDIN_FILE: &str = "<stdin>";

/// 运行过程中值得报出去的事。
///
/// 与桌面端推给前端的三段式（start → case → end）同形：外壳拿 `Start` 打表头、
/// 拿 `Case` 逐行打进度，收尾的统计从返回的报告里取。MCP 侧整个传 `None`。
pub enum Event<'a> {
    Start { total: usize, environment: &'a apicase_core::report::EnvironmentInfo },
    Case(&'a CaseResult),
}

/// 进度回调。**这是 ops 层唯一的对外输出通道**——它自己不 print（见模块文档）。
pub type OnEvent = Arc<dyn Fn(Event<'_>) + Send + Sync>;

/// 跑一轮。
///
/// 目标解析、参数组装、报告落盘全在这里；执行本身交给 `runner::run_batch`。
/// 给了 `content` 就跑那段内联 YAML（`apicase run -` / MCP 的 `content` 参数）。
pub async fn run(
    ws: &Workspace,
    req: &RunRequest,
    on_event: Option<OnEvent>,
    cancel: Cancel,
) -> Result<RunOutcome, String> {
    if let Some(content) = req.content.clone() {
        return run_content(ws, req, &content, on_event, cancel).await;
    }
    let (targets, rel_targets) = resolve_targets(ws, req)?;
    let opts = build_opts(ws, req);
    let meta = BatchMeta {
        workspace: ws.info(),
        tool_version: tool_version(),
        // 报告头的运行参数从 opts 派生，不并列写第二遍——半年后回看一份失败报告时，
        // 「当时用的哪套环境、截断阈值多少」直接决定结论能不能信
        options: RunOptions {
            targets: rel_targets,
            recursive: req.recursive,
            environment: opts.environment.name.clone(),
            concurrency: opts.concurrency,
            stop_on_failure: opts.stop_on_failure,
            max_body_bytes: opts.max_body_bytes,
            continue_on_assertion_failure: opts.continue_on_assertion_failure,
        },
    };

    // 报告与 cookie jar 都含明文凭据，开跑前先把 .apicase/ 挡在版本库外。
    // 未锚定的目录不碰——那可能是 /tmp 或别人的项目，往里塞 .gitignore 是侵入
    if !ws.is_scratch() {
        ws.ensure_gitignore();
    }
    let writer = open_writer(ws, req, &meta.options.targets)?;

    if let Some(f) = on_event.as_ref() {
        f(Event::Start { total: targets.len(), environment: &opts.environment });
    }

    // 只跑部分 step 时先裁剪模型，故走单 case 路径；其余走 run_batch（并发、取消都在那儿）
    let report = if req.steps.is_empty() {
        let progress = progress_fn(on_event, writer.clone());
        runner::run_batch(targets, meta, opts, progress, cancel).await
    } else {
        run_selected_steps(&targets, meta, opts, req, on_event, writer.clone(), cancel).await
    };

    if let Some(w) = writer.as_ref() {
        w.write_now(&report);
    }
    Ok(RunOutcome { report, report_path: writer.map(|w| PathBuf::from(w.path())) })
}

/// 目标 → 待跑清单 + 记进报告的相对路径。
fn resolve_targets(ws: &Workspace, req: &RunRequest) -> Result<(Vec<BatchTarget>, Vec<String>), String> {
    if req.content.is_some() {
        return Ok((Vec::new(), vec![STDIN_FILE.into()]));
    }

    // 没给目标 = 整个工作空间。**但未锚定时不许这么干**：那时的「根」只是当前目录，
    // 在 ~ 或 / 底下敲一句 `apicase run` 会递归扫掉整个主目录，
    // 把碰巧同名的 .yml 全当用例跑一遍——那是能造成真实伤害的（我们发的是真 HTTP 写请求）。
    if req.targets.is_empty() && ws.is_scratch() {
        return Err(format!(
            "{} 不是工作空间（没有 {}），不能省略目标——请指明要跑哪个文件或目录，\
             或用 apicase init 把它声明为工作空间",
            ws.root.display(),
            workspace::CONFIG_FILE
        ));
    }
    let roots: Vec<PathBuf> =
        if req.targets.is_empty() { vec![ws.root.clone()] } else { req.targets.clone() };
    for t in &roots {
        if !t.exists() {
            return Err(format!("目标不存在：{}", t.display()));
        }
    }

    let files = discover::find_all(&roots, req.recursive);
    if files.is_empty() {
        return Err(format!(
            "{} 里没有可运行的用例（找的是 .yml / .yaml，不含 {}）",
            roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("、"),
            workspace::CONFIG_FILE
        ));
    }

    let targets = files
        .iter()
        .map(|p| BatchTarget { file: ws.rel(p), path: p.to_string_lossy().into_owned() })
        .collect();
    Ok((targets, roots.iter().map(|p| ws.rel(p)).collect()))
}

fn build_opts(ws: &Workspace, req: &RunRequest) -> RunOpts {
    let mut env = ws.env_info(req.env.as_deref());
    // --var 是「往活动环境里追加 / 覆盖」，仍会被 case 级 vars 盖掉。
    // 语义与「环境变量」一致，可解释，且执行内核一行不改。
    for (k, v) in &req.vars {
        env.vars.insert(k.clone(), v.clone());
    }

    let mut o = ws.run_opts(env, req.proxy.clone());
    // 并行度默认已由 ws.run_opts 装好（工作空间设置）；-j 是这一次的临时覆盖
    if let Some(n) = req.concurrency {
        o.concurrency = n.clamp(1, apicase_core::model::MAX_CONCURRENCY);
    }
    o.stop_on_failure = req.stop_on_failure;
    if let Some(c) = req.continue_on_assertion_failure {
        o.continue_on_assertion_failure = c;
    }
    if let Some(opts) = o.client.options.as_mut() {
        if let Some(ms) = req.timeout_ms {
            opts.timeout_ms = (ms > 0).then_some(ms);
        }
        if req.insecure {
            opts.verify_ssl = Some(false);
        }
    }
    o
}

/// 按 `ReportSink` 决定要不要落盘，顺带把目录建好。
///
/// **建目录失败在这里就报出来**，而不是等到第一次落盘时被静默吞掉——
/// 跑完一轮才发现报告没写成，那一轮就白跑了。
fn open_writer(ws: &Workspace, req: &RunRequest, rel_targets: &[String]) -> Result<Option<ReportWriter>, String> {
    let path = match &req.report {
        ReportSink::None => return Ok(None),
        ReportSink::Path(p) => p.clone(),
        // 未锚定时不自动落盘：那个目录没被声明成工作空间，不该凭空长出 .apicase/。
        // 显式 `--report <文件>` 仍然照办——那是用户点名要的。
        ReportSink::Auto if ws.is_scratch() => return Ok(None),
        ReportSink::Auto => {
            // 跑整个工作空间时目标是根，相对路径为空——用工作空间名，
            // 否则一堆只有时间戳的报告文件放在一起，光看名字分不出跑的是什么。
            // 多目标则是 `<首个目标>等N项`：只写第一个会让人以为只跑了它
            let usable: Vec<&str> =
                rel_targets.iter().map(String::as_str).filter(|s| !s.is_empty() && *s != ".").collect();
            let ws_name = ws.name();
            let name = if usable.is_empty() {
                workspace::report_file_name(&crate::local_stamp(), &ws_name)
            } else {
                workspace::report_file_name_multi(&crate::local_stamp(), &usable)
            };
            ws.reports_dir().join(name)
        }
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建报告目录 {} 失败：{e}", dir.display()))?;
    }
    Ok(Some(ReportWriter::new(path.to_string_lossy().into_owned())))
}

/// 把「每完成一个 case」的回调与周期落盘挂到 runner 的进度回调上。
///
/// runner 的回调在开跑与收尾时也会来（那两次 cases 没有新增），靠计数区分，
/// 否则最后一个 case 会被通知两遍。
fn progress_fn(on_event: Option<OnEvent>, writer: Option<ReportWriter>) -> Option<runner::ProgressFn> {
    if on_event.is_none() && writer.is_none() {
        return None;
    }
    let seen = std::sync::atomic::AtomicUsize::new(0);
    Some(Arc::new(move |r: &RunReport| {
        let n = r.cases.len();
        if n > seen.swap(n, std::sync::atomic::Ordering::SeqCst) {
            if let (Some(f), Some(c)) = (on_event.as_ref(), r.cases.last()) {
                f(Event::Case(c));
            }
        }
        if let Some(w) = writer.as_ref() {
            w.maybe_write(r);
        }
    }))
}

/// `--step`：先把 case 裁剪到「点名的 step + 它们的上游依赖」，再跑。
///
/// 走单 case 路径而不是 `run_batch`，因为裁剪要在解析之后、执行之前插进去。
/// 这条路径不支持并发（`--step` 本就是「盯着这一条链看」的用法，并发没有意义）。
#[allow(clippy::too_many_arguments)]
async fn run_selected_steps(
    targets: &[BatchTarget],
    meta: BatchMeta,
    opts: RunOpts,
    req: &RunRequest,
    on_event: Option<OnEvent>,
    writer: Option<ReportWriter>,
    cancel: Cancel,
) -> RunReport {
    let t0 = apicase_core::util::now_ms();
    let mut report = RunReport {
        schema_version: apicase_core::report::REPORT_SCHEMA_VERSION,
        tool: apicase_core::report::ToolInfo { name: "apicase".into(), version: meta.tool_version },
        started_at: apicase_core::util::iso8601(t0),
        finished_at: None,
        duration_ms: 0,
        status: RunStatus::Running,
        workspace: meta.workspace,
        environment: opts.environment.clone(),
        options: meta.options,
        summary: Default::default(),
        cases: Vec::new(),
    };

    for t in targets {
        if cancel.is_cancelled() {
            break;
        }
        let result = match run_one_selected(t, &opts, req, &cancel).await {
            Ok(r) => r,
            Err(reason) => skipped_case(&t.file, reason, t0),
        };
        let stop = opts.stop_on_failure
            && matches!(
                result.status,
                apicase_core::report::CaseStatus::Failed | apicase_core::report::CaseStatus::Error
            );
        if let Some(f) = on_event.as_ref() {
            f(Event::Case(&result));
        }
        report.cases.push(result);
        report.summary = apicase_core::report::RunSummary::of(&report.cases);
        report.duration_ms = apicase_core::util::now_ms().saturating_sub(t0);
        if let Some(w) = writer.as_ref() {
            w.maybe_write(&report);
        }
        if stop {
            break;
        }
    }

    apicase_core::cookie::flush_all();
    let end = apicase_core::util::now_ms();
    report.status = if cancel.is_cancelled() { RunStatus::Cancelled } else { RunStatus::Done };
    report.finished_at = Some(apicase_core::util::iso8601(end));
    report.duration_ms = end.saturating_sub(t0);
    report
}

async fn run_one_selected(
    t: &BatchTarget,
    opts: &RunOpts,
    req: &RunRequest,
    cancel: &Cancel,
) -> Result<CaseResult, String> {
    let text = read_text(Path::new(&t.path))?;
    let analyzed = yaml::analyze_case(&text);
    let mut case = analyzed
        .case
        .filter(|_| analyzed.valid)
        .ok_or_else(|| analyzed.error.unwrap_or_else(|| "不是有效的用例".into()))?;

    let keep = runner::with_dependencies(&case.requests, &req.steps);
    if keep.is_empty() {
        // 这个文件里没有点名的 step——是常态（跑一个目录时只有一两个文件有它），
        // 说清楚比留一条无声的空记录强
        return Err(format!("没有名为 {} 的请求", req.steps.join(" / ")));
    }
    case.requests = keep.into_iter().map(|i| case.requests[i].clone()).collect();
    Ok(runner::run_case_model(&case, &t.file, opts, cancel).await)
}

fn skipped_case(file: &str, reason: String, at: u64) -> CaseResult {
    CaseResult {
        file: file.to_string(),
        name: file.rsplit(['/', '\\']).next().unwrap_or(file).to_string(),
        status: apicase_core::report::CaseStatus::Skipped,
        skip_reason: Some(reason),
        started_at: apicase_core::util::iso8601(at),
        duration_ms: 0,
        steps: Vec::new(),
    }
}

/// 跑一段内联 YAML（`apicase run -` / MCP 的 `content`）。
///
/// 与文件路径分开是因为它没有文件：报告里记 `<stdin>`，也不需要发现与遍历。
async fn run_content(
    ws: &Workspace,
    req: &RunRequest,
    content: &str,
    on_event: Option<OnEvent>,
    cancel: Cancel,
) -> Result<RunOutcome, String> {
    let opts = build_opts(ws, req);
    if let Some(f) = on_event.as_ref() {
        f(Event::Start { total: 1, environment: &opts.environment });
    }
    let t0 = apicase_core::util::now_ms();
    let case = runner::run_case(content, STDIN_FILE, &opts, &cancel).await;
    if let Some(f) = on_event.as_ref() {
        f(Event::Case(&case));
    }
    apicase_core::cookie::flush_all();
    let end = apicase_core::util::now_ms();

    let mut report = RunReport {
        schema_version: apicase_core::report::REPORT_SCHEMA_VERSION,
        tool: apicase_core::report::ToolInfo { name: "apicase".into(), version: tool_version() },
        started_at: apicase_core::util::iso8601(t0),
        finished_at: Some(apicase_core::util::iso8601(end)),
        duration_ms: end.saturating_sub(t0),
        status: if cancel.is_cancelled() { RunStatus::Cancelled } else { RunStatus::Done },
        workspace: ws.info(),
        environment: opts.environment.clone(),
        options: RunOptions {
            targets: vec![STDIN_FILE.into()],
            recursive: false,
            environment: opts.environment.name.clone(),
            concurrency: 1,
            stop_on_failure: opts.stop_on_failure,
            max_body_bytes: opts.max_body_bytes,
            continue_on_assertion_failure: opts.continue_on_assertion_failure,
        },
        summary: Default::default(),
        cases: vec![case],
    };
    report.summary = apicase_core::report::RunSummary::of(&report.cases);

    let writer = open_writer(ws, req, &[STDIN_FILE.into()])?;
    if let Some(w) = writer.as_ref() {
        w.write_now(&report);
    }
    Ok(RunOutcome { report, report_path: writer.map(|w| PathBuf::from(w.path())) })
}
