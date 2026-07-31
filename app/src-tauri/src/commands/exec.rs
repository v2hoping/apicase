//! 执行相关的命令：case 解析、调试运行、批量运行、报告渲染。
//!
//! 这个模块**没有任何执行语义**——全部转发给 `apicase-core`。它只做三件事：
//! 把前端传来的 JSON 变成 core 的类型、把 core 的产物变回 JSON、
//! 把批量运行的进度以 Tauri 事件推给前端。
//!
//! 之所以刻意保持得这么薄：将来的 `apicase run` CLI 会平行地调同一批 core 函数，
//! 一旦有语义漏进这一层，CLI 就得把它重写一遍——那正是这次改造要消灭的东西。

use apicase_core::render;
use apicase_core::report::{CaseResult, RunReport, RunStatus, RunSummary};
use apicase_core::runner::{self, BatchMeta, BatchTarget, Cancel, RunOpts};
use apicase_core::yaml::{self, AnalyzeResult, Environments};
use apicase_core::{Case, Step, WorkspaceSettings};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

// ── case 解析 / 序列化 ──────────────────────────────

/// 校验并解析 case 文本（前端据此决定用结构化视图还是纯文本兜底）。
#[tauri::command]
pub fn analyze_case(text: String) -> AnalyzeResult {
    yaml::analyze_case(&text)
}

/// case 模型 → YAML 文本（保存用）。
#[tauri::command]
pub fn dump_case(case: Case) -> String {
    yaml::dump_case(&case)
}

/// `application.yml` 的解析结果：环境表 + 工作空间设置一次取回。
/// 前端每次读这个文件都要这两样，分成两个命令等于两次 IPC + 两次 YAML 解析。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub environment: Environments,
    pub settings: WorkspaceSettings,
}

#[tauri::command]
pub fn parse_app_config(text: String) -> AppConfig {
    AppConfig { environment: yaml::parse_environments(&text), settings: yaml::parse_settings(&text) }
}

/// 把可视化编辑的 environment / settings 写回 `application.yml`（保留其它顶层键）。
#[tauri::command]
pub fn dump_app_config(
    base_text: String,
    environment: Environments,
    settings: Option<WorkspaceSettings>,
) -> String {
    yaml::dump_application_config(&base_text, &environment, settings.as_ref())
}

// ── 报告 ────────────────────────────────────────────

/// 报告空壳（apicase 内嵌 iframe 用；与落盘报告共用同一套渲染实现）。
#[tauri::command]
pub fn report_shell() -> String {
    render::report_shell()
}

/// 从报告 HTML 读回结构化数据（点开历史 `report.html` 时用）。
/// 认不出就返回 null，前端据此降级为普通文本视图。
#[tauri::command]
pub fn parse_report(text: String) -> Option<RunReport> {
    render::parse_report_html(&text)
}

// ── 调试运行（单步）─────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepRun {
    pub step: apicase_core::report::StepResult,
    /// **未脱敏**的输出，供前端在同一个 case 内透传给下游 step
    pub outputs: BTreeMap<String, serde_json::Value>,
}

/// 执行单个 step —— 界面上的「发送 / ▶ 运行」走这里。
///
/// 上下文由前端传入而非后端保存：它是「当前打开的这个标签页的运行态」，
/// 生命周期跟着 UI 走（切标签、改用例都会重置），放后端反而要同步两份状态。
#[tauri::command]
pub async fn run_step(
    step: Step,
    vars: BTreeMap<String, serde_json::Value>,
    steps: HashMap<String, BTreeMap<String, serde_json::Value>>,
    opts: RunOpts,
) -> Result<StepRun, String> {
    let ctx = apicase_core::vars::RunContext { vars, steps };
    let (step, outputs) = runner::run_step(&step, &ctx, &opts).await;
    Ok(StepRun { step, outputs })
}

/// 按 `dependsOn` 排出可依次执行的顺序（返回下标）。
///
/// 「运行全部」由前端驱动循环（要逐步刷新界面），但**排序规则只有 core 一份**——
/// 成环怎么兜底、依赖指向不存在的 step 怎么处理，这些边界不该在两处各写一遍。
#[tauri::command]
pub fn topo_order(steps: Vec<Step>) -> Vec<usize> {
    runner::topo_order(&steps)
}

// ── 批量运行 ────────────────────────────────────────

/// 在跑的批量运行：runId → 取消令牌。
#[derive(Default)]
pub struct RunState(Mutex<HashMap<String, Cancel>>);

impl RunState {
    /// 锁中毒不该让运行功能报废（同 `ClientPool::lock` 的理由）。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Cancel>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 推给前端的运行进度。
///
/// **按增量推而不是每次整份重发**：一份跑了 200 个用例的报告可达数 MB，
/// 每完成一个就把整份序列化过一次 IPC，光是编解码就能把界面拖卡。
/// 前端拿 `start` 里的报告头打底，之后逐个 `case` 追加即可。
/// `rename_all` 只管 variant 名，**字段名要 `rename_all_fields`** ——
/// 少了后者，前端拿到的是 `duration_ms`，TS 侧读 `durationMs` 就是 undefined，
/// 表现为"进度条一动不动"，而且没有任何报错。下方有测试钉住这一点。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RunEvent {
    /// 开跑：报告头（`cases` 为空）
    Start { report: Box<RunReport>, total: usize },
    /// 完成一个 case
    Case { case: Box<CaseResult>, summary: RunSummary, duration_ms: u64 },
    /// 收尾
    End { status: RunStatus, finished_at: Option<String>, summary: RunSummary, duration_ms: u64 },
}

fn event_name(run_id: &str) -> String {
    format!("run://progress/{run_id}")
}

/// 批量运行的参数。`report_file` 给了就周期落盘（运行中可用浏览器打开看部分结果）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchArgs {
    pub run_id: String,
    pub targets: Vec<BatchTarget>,
    pub meta: BatchMeta,
    pub opts: RunOpts,
    #[serde(default)]
    pub report_file: Option<String>,
}

/// 启动一次批量运行，跑完返回最终报告；过程中以事件推进度。
#[tauri::command]
pub async fn run_batch(app: AppHandle, state: State<'_, RunState>, args: BatchArgs) -> Result<RunReport, String> {
    let BatchArgs { run_id, targets, meta, opts, report_file } = args;
    let cancel = Cancel::new();
    state.lock().insert(run_id.clone(), cancel.clone());

    // 报告目录可能是三级的 `.apicase/reports/<时间戳>/`，先建出来
    if let Some(f) = report_file.as_deref() {
        if let Some(dir) = std::path::Path::new(f).parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建报告目录失败: {e}"))?;
        }
    }

    let writer = report_file.map(ReportWriter::new);
    let seen = Arc::new(AtomicUsize::new(0));
    let total = targets.len();
    let progress = {
        let (app, ev, seen, writer) = (app.clone(), event_name(&run_id), seen.clone(), writer.clone());
        Arc::new(move |r: &RunReport| {
            let n = r.cases.len();
            let prev = seen.swap(n, Ordering::SeqCst);
            let event = if n > prev {
                // 一次回调只会新增一个 case（runner 每完成一个就回调一次）
                RunEvent::Case {
                    case: Box::new(r.cases[n - 1].clone()),
                    summary: r.summary,
                    duration_ms: r.duration_ms,
                }
            } else if r.status == RunStatus::Running {
                RunEvent::Start { report: Box::new(strip_cases(r)), total }
            } else {
                RunEvent::End {
                    status: r.status,
                    finished_at: r.finished_at.clone(),
                    summary: r.summary,
                    duration_ms: r.duration_ms,
                }
            };
            let _ = app.emit(&ev, &event);
            if let Some(w) = writer.as_ref() {
                w.maybe_write(r);
            }
        })
    };

    let report = runner::run_batch(targets, meta, opts, Some(progress), cancel).await;
    if let Some(w) = writer.as_ref() {
        w.write_now(&report);
    }
    state.lock().remove(&run_id);
    Ok(report)
}

/// 取消一次运行（在 case 边界生效；已发出的 HTTP 不中断）。
#[tauri::command]
pub fn cancel_run(state: State<'_, RunState>, run_id: String) {
    if let Some(c) = state.lock().get(&run_id) {
        c.cancel();
    }
}

fn strip_cases(r: &RunReport) -> RunReport {
    RunReport { cases: Vec::new(), ..r.clone() }
}

/// 周期写盘：运行中每 ≥1s 整份覆写一次报告，用户中途用浏览器打开刷新就能看到部分结果。
///
/// 整份重渲染就是几毫秒的字符串拼接，不值得为省这点开销去做增量；
/// 但**写盘要串行**——两次写交错会让报告文件出现半截内容。
#[derive(Clone)]
struct ReportWriter {
    path: String,
    last: Arc<Mutex<std::time::Instant>>,
}

impl ReportWriter {
    fn new(path: String) -> Self {
        // 初始时间往前推，保证第一次回调就会落盘
        let past = std::time::Instant::now() - std::time::Duration::from_secs(60);
        Self { path, last: Arc::new(Mutex::new(past)) }
    }

    fn maybe_write(&self, r: &RunReport) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        *last = std::time::Instant::now();
        // 持锁写盘 = 天然串行，不会写出半截文件
        self.write_locked(r);
    }

    fn write_now(&self, r: &RunReport) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        *last = std::time::Instant::now();
        self.write_locked(r);
    }

    /// 写盘失败不中断运行——结果仍在应用内可见，报告只是少了一份归档。
    fn write_locked(&self, r: &RunReport) {
        let _ = std::fs::write(&self.path, render::render_html(r));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apicase_core::report::StepStatus;

    /// 命令层是**转发**，不是重新实现——这些测试盯的是"接线对不对"：
    /// 参数进得来、产物出得去、序列化形状与前端 TS 类型对得上。
    /// 执行语义本身由 apicase-core 的单测覆盖，这里不重复。

    #[test]
    fn case_commands_round_trip() {
        let text = "apicase: v0.1\nname: 冒烟\nsteps:\n  - id: a\n    protocol: http\n    request:\n      method: GET\n      url: http://x/a\n";
        let r = analyze_case(text.into());
        assert!(r.valid, "{r:?}");
        let case = r.case.expect("应有 case");
        assert_eq!(dump_case(case), text, "解析再序列化应逐字回到原文");

        // 认不出的内容要给出原因，前端据此回退纯文本视图
        let bad = analyze_case("这不是: [有效\n  yaml".into());
        assert!(!bad.valid);
        assert!(bad.error.is_some());
    }

    #[test]
    fn app_config_commands_round_trip() {
        let text = "environment:\n  dev:\n    baseUrl: http://x\nsettings:\n  timeoutMs: 5000\ncustom:\n  keep: me\n";
        let cfg = parse_app_config(text.into());
        assert_eq!(cfg.settings.timeout_ms, 5000);
        assert_eq!(cfg.environment.keys().collect::<Vec<_>>(), vec!["dev"]);

        let out = dump_app_config(text.into(), cfg.environment.clone(), Some(cfg.settings.clone()));
        assert!(out.contains("custom:"), "未知顶层键要保留：\n{out}");
        assert_eq!(parse_app_config(out).settings, cfg.settings);
    }

    #[test]
    fn topo_order_forwards_to_core() {
        let case = apicase_core::yaml::parse_case(
            "apicase: v0.1\nsteps:\n  - id: b\n    dependsOn:\n      - a\n    request:\n      url: http://x\n  - id: a\n    request:\n      url: http://x\n",
        )
        .expect("应能解析");
        let order = topo_order(case.requests.clone());
        let ids: Vec<&str> = order.iter().map(|&i| case.requests[i].id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    /// 报告命令与前端的契约：空壳能被 iframe 直接加载，落盘报告能被读回。
    #[test]
    fn report_commands_pair_up() {
        let shell = report_shell();
        assert!(shell.starts_with("<!doctype html>"));
        assert!(parse_report(shell).is_none(), "空壳没有数据，应判为不是报告");
        assert!(parse_report("<html>普通网页</html>".into()).is_none());
    }

    /// run_step 的错误路径：连不上的地址应落成 error（不是 failed），
    /// 且**将要发送的报文仍被记录**——否则一句"请求失败"连打到哪都看不出来。
    #[tokio::test]
    async fn run_step_reports_transport_errors() {
        let step = apicase_core::yaml::parse_case(
            "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: http://127.0.0.1:1/nope\n",
        )
        .expect("应能解析")
        .requests
        .remove(0);

        let opts = RunOpts {
            environment: apicase_core::report::EnvironmentInfo {
                name: "t".into(),
                vars: Default::default(),
            },
            concurrency: 1,
            stop_on_failure: false,
            redact: false,
            max_body_bytes: usize::MAX,
            // 开发机常设 HTTPS_PROXY，不绕开就到不了本地地址
            client: apicase_core::http::ClientConfig {
                proxy: Some(apicase_core::http::ProxyConfig { mode: "none".into(), url: None }),
                options: None,
            },
        };
        let out = run_step(step, Default::default(), Default::default(), opts).await.expect("命令本身不该失败");
        assert_eq!(out.step.status, StepStatus::Error);
        assert!(out.step.error.is_some());
        assert_eq!(out.step.request.expect("应记录将要发送的报文").url, "http://127.0.0.1:1/nope");
        assert!(out.outputs.is_empty());
    }

    /// 进度事件的**序列化形状**必须与前端 `RunEvent` 的可辨联合对得上：
    /// 靠 `kind` 分流，字段名 camelCase。对不上就是运行中界面一片空白。
    #[test]
    fn run_events_match_the_frontend_shape() {
        let ev = RunEvent::End {
            status: RunStatus::Done,
            finished_at: Some("2026-07-30T00:00:00.000Z".into()),
            summary: RunSummary::default(),
            duration_ms: 12,
        };
        let json = serde_json::to_value(&ev).expect("序列化");
        assert_eq!(json["kind"], "end");
        assert_eq!(json["status"], "done");
        assert_eq!(json["durationMs"], 12);
        assert!(json.get("finishedAt").is_some(), "字段名必须是 camelCase: {json}");

        let ev = RunEvent::Case {
            case: Box::new(CaseResult {
                file: "a.yml".into(),
                name: "a".into(),
                status: apicase_core::report::CaseStatus::Passed,
                skip_reason: None,
                started_at: "2026-07-30T00:00:00.000Z".into(),
                duration_ms: 1,
                steps: vec![],
            }),
            summary: RunSummary::default(),
            duration_ms: 1,
        };
        let json = serde_json::to_value(&ev).expect("序列化");
        assert_eq!(json["kind"], "case");
        assert_eq!(json["case"]["file"], "a.yml");
    }

    /// 事件名两边必须一字不差，否则前端订阅了个永远不响的频道
    #[test]
    fn event_name_matches_the_frontend_subscription() {
        assert_eq!(event_name("1784073600000"), "run://progress/1784073600000");
    }

    /// 周期写盘：第一次立刻落盘（用户中途就能用浏览器打开看），随后 1s 内不重复写
    #[test]
    fn report_writer_throttles_after_the_first_write() {
        let path = std::env::temp_dir().join("apicase-writer-test.html");
        let _ = std::fs::remove_file(&path);
        let w = ReportWriter::new(path.to_string_lossy().into_owned());
        let r = RunReport {
            schema_version: apicase_core::report::REPORT_SCHEMA_VERSION,
            tool: apicase_core::report::ToolInfo { name: "apicase".into(), version: "0".into() },
            started_at: "2026-07-30T00:00:00.000Z".into(),
            finished_at: None,
            duration_ms: 0,
            status: RunStatus::Running,
            workspace: apicase_core::report::WorkspaceInfo { name: "w".into(), root: "/w".into() },
            environment: apicase_core::report::EnvironmentInfo { name: "e".into(), vars: Default::default() },
            options: apicase_core::report::RunOptions {
                targets: vec![],
                recursive: true,
                environment: "e".into(),
                concurrency: 1,
                stop_on_failure: false,
                redact: true,
                max_body_bytes: 1024,
            },
            summary: RunSummary::default(),
            cases: vec![],
        };
        w.maybe_write(&r);
        assert!(path.is_file(), "第一次回调就该落盘");
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();

        w.maybe_write(&r); // 1s 内的第二次应被节流掉
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first);

        w.write_now(&r); // 收尾那次强制写
        assert!(std::fs::read_to_string(&path).unwrap().contains("apicase-report"));
        let _ = std::fs::remove_file(&path);
    }
}
