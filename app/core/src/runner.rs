//! 执行引擎：`run_step` / `run_case` / `run_batch` 三级。
//!
//! **调试运行与回归运行走同一份实现**——界面上点「发送」是 `run_step`，
//! 目录批量运行是 `run_batch`，中间只差脱敏与截断这两个开关。一份执行语义，
//! 因此不会出现"界面里跑过了、批量跑却挂了"这类两套实现必然产生的漂移。
//!
//! # 三条不变量
//!
//! - **变量隔离**：每个 case 一份独立 `RunContext`（environment + 该 case 的 `vars`），
//!   **case 之间不共享 outputs**。outputs 只在 case 内部的 step 间流动，
//!   否则一开并发就是竞态。
//! - **`failed` ≠ `error`**：断言没过是被测服务的问题，请求本身失败是环境或用例的问题。
//! - **取消在 case 边界生效**：已发出的 HTTP 不中断，避免服务端收到半截请求。

use crate::assert::{eval_assertions, extract_outputs, RespView};
use crate::auth::send_with_auth;
use crate::http::ClientConfig;
use crate::model::Step;
use crate::redact::*;
use crate::report::*;
use crate::request::RequestBody;
use crate::util::{iso8601, now_ms};
use crate::vars::{resolve_http, RunContext};
use crate::yaml;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024;

/// 取消令牌。在 case 与 step 边界被检查——**不打断已发出的 HTTP**，
/// 半截请求对被测服务是一种伤害，而等一个请求收尾最多就是几秒。
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// 一次执行的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOpts {
    pub environment: EnvironmentInfo,
    /// case 之间的并发度；1 = 串行（默认）。case 内部的 step 恒按拓扑序串行。
    #[serde(default = "one")]
    pub concurrency: u32,
    #[serde(default)]
    pub stop_on_failure: bool,
    /// 报告会被转发 / 归档，故批量运行默认开；调试运行关掉——响应区要看真实内容
    #[serde(default)]
    pub redact: bool,
    #[serde(default = "default_max_body")]
    pub max_body_bytes: usize,
    #[serde(default)]
    pub client: ClientConfig,
}

fn one() -> u32 {
    1
}
fn default_max_body() -> usize {
    DEFAULT_MAX_BODY_BYTES
}

impl RunOpts {
    /// 批量运行的默认参数：串行、失败继续、**脱敏开启**、报文体截断 64KB。
    pub fn for_batch(environment: EnvironmentInfo) -> Self {
        Self {
            environment,
            concurrency: 1,
            stop_on_failure: false,
            redact: true,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            client: ClientConfig::default(),
        }
    }

    /// 调试运行的默认参数：**不脱敏、不截断**——响应区要看的就是真实内容，
    /// 脱敏与截断只在写进报告时才有意义。
    pub fn for_debug(environment: EnvironmentInfo) -> Self {
        Self {
            environment,
            concurrency: 1,
            stop_on_failure: false,
            redact: false,
            max_body_bytes: usize::MAX,
            client: ClientConfig::default(),
        }
    }
}

// ── 单步 ────────────────────────────────────────────

/// 拓扑序：按 `dependsOn` 把 step 排成可依次执行的顺序。
/// **成环时按出现序兜底，不死循环**——环是配置错误，但不该让运行卡死。
pub fn topo_order(steps: &[Step]) -> Vec<usize> {
    let by_id: HashMap<&str, usize> =
        steps.iter().enumerate().map(|(i, s)| (s.id.as_str(), i)).collect();
    let mut visited = vec![false; steps.len()];
    let mut on_stack = vec![false; steps.len()];
    let mut out = Vec::with_capacity(steps.len());
    for i in 0..steps.len() {
        visit(i, steps, &by_id, &mut visited, &mut on_stack, &mut out);
    }
    out
}

fn visit(
    i: usize,
    steps: &[Step],
    by_id: &HashMap<&str, usize>,
    visited: &mut [bool],
    on_stack: &mut [bool],
    out: &mut Vec<usize>,
) {
    if visited[i] || on_stack[i] {
        return;
    }
    on_stack[i] = true;
    for dep in &steps[i].depends_on {
        if let Some(&j) = by_id.get(dep.as_str()) {
            visit(j, steps, by_id, visited, on_stack, out);
        }
    }
    on_stack[i] = false;
    visited[i] = true;
    out.push(i);
}

/// 收集本步可见的全部凭据字面值。
///
/// 三个来源缺一不可：**环境变量**、**case 级 vars**、以及**上游 step 提取出的 outputs**
/// （登录拿到 token 再传给下游，是最常见的凭据流动路径）。
fn secrets_in_scope(ctx: &RunContext, opts: &RunOpts) -> Vec<String> {
    let mut out = secret_values_of_strings(&opts.environment.vars, opts.redact);
    out.extend(secret_values(ctx.vars.iter(), opts.redact));
    for outputs in ctx.steps.values() {
        out.extend(secret_values_of_outputs(outputs, opts.redact));
    }
    out.sort();
    out.dedup();
    out
}

/// 执行单个 step：变量透传 → 发送 → 提取 outputs → 评估断言。
///
/// 返回的第二项是**未脱敏的 outputs**——下游 step 要拿它发真实请求；
/// 写进报告的那份（`StepResult.outputs`）已经掩码过了。
pub async fn run_step(step: &Step, ctx: &RunContext, opts: &RunOpts) -> (StepResult, BTreeMap<String, Value>) {
    let t0 = now_ms();
    let secrets = secrets_in_scope(ctx, opts);
    let clean = |b: Option<&str>| clean_body(b, &secrets, opts.redact, opts.max_body_bytes);

    let mut result = StepResult {
        id: step.id.clone(),
        status: StepStatus::Error,
        duration_ms: 0,
        request: None,
        response: None,
        outputs: BTreeMap::new(),
        assertions: Vec::new(),
        error: None,
    };

    let resolved = resolve_http(&step.http, ctx);
    match send_with_auth(&resolved, &opts.client).await {
        Ok((sent, resp)) => {
            result.request = Some(record_request(&sent, &secrets, opts));
            // 输出提取与断言看的是同一个响应切面（路径语法也是同一套）
            let view = RespView { status: resp.status, headers: &resp.headers, body: &resp.body };
            let outputs = extract_outputs(&step.outputs, &view);
            let assertions = eval_assertions(&step.assertions, &view);
            result.status = if assertions.iter().all(|a| a.ok) { StepStatus::Passed } else { StepStatus::Failed };
            result.response = Some(ResponseRecord {
                status: resp.status,
                status_text: resp.status_text,
                headers: redact_headers(&resp.headers, opts.redact),
                body: clean(Some(&resp.body)),
                elapsed_ms: resp.elapsed_ms,
            });
            result.outputs = redact_outputs(&outputs, opts.redact);
            // 断言的 actual 直接来自响应体，凭据在这里同样会露出来
            result.assertions = redact_assertions(assertions, &secrets, opts.redact);
            result.duration_ms = now_ms().saturating_sub(t0);
            (result, outputs)
        }
        Err(e) => {
            // 请求没发出去（或认证前置步骤失败）也要把**将要发送**的报文记进报告——
            // 否则一条 "请求失败" 的错误连打到哪个 URL 都看不出来。
            result.request = Some(record_request(&crate::request::build(&resolved), &secrets, opts));
            result.error = Some(scrub_secrets(&e, &secrets));
            result.duration_ms = now_ms().saturating_sub(t0);
            (result, BTreeMap::new())
        }
    }
}

fn record_request(req: &crate::request::HttpRequest, secrets: &[String], opts: &RunOpts) -> RequestRecord {
    RequestRecord {
        method: req.method.clone(),
        // URL 也要清洗：API Key 放 query 时凭据就在这里
        url: scrub_secrets(&req.url, secrets),
        headers: redact_headers(&req.headers, opts.redact),
        body: match &req.body {
            Some(RequestBody::Text(t)) => clean_body(Some(t), secrets, opts.redact, opts.max_body_bytes),
            // 文件与表单体没有可展示的文本，记个说明比记一片空白强
            Some(RequestBody::File(p)) => BodyRecord {
                preview: Some(format!("<二进制文件：{p}>")),
                bytes: 0,
                truncated: false,
            },
            Some(RequestBody::Form(parts)) => BodyRecord {
                preview: Some(format!("<multipart/form-data：{} 个字段>", parts.len())),
                bytes: 0,
                truncated: false,
            },
            None => BodyRecord::absent(),
        },
    }
}

// ── 单个 case ───────────────────────────────────────

/// 汇总一个 case 内各 step 的状态：任一 error 即 error，任一 failed 即 failed。
fn rollup(steps: &[StepResult]) -> CaseStatus {
    if steps.iter().any(|s| s.status == StepStatus::Error) {
        return CaseStatus::Error;
    }
    if steps.iter().any(|s| s.status == StepStatus::Failed) {
        return CaseStatus::Failed;
    }
    CaseStatus::Passed
}

fn file_name_of(file: &str) -> String {
    file.rsplit(['/', '\\']).next().unwrap_or(file).to_string()
}

fn skipped(file: &str, reason: impl Into<String>, at: u64) -> CaseResult {
    CaseResult {
        file: file.to_string(),
        name: file_name_of(file),
        status: CaseStatus::Skipped,
        skip_reason: Some(reason.into()),
        started_at: iso8601(at),
        duration_ms: 0,
        steps: Vec::new(),
    }
}

/// 执行一个 case（**文本**→ 结果）。
///
/// 接收文本而非路径：批量运行期间用户可能正在编辑用例，跑的应当是
/// 「开跑那一刻读到的内容」，由调用方一次性读好；同时让本函数保持纯粹、便于测试。
pub async fn run_case(text: &str, file: &str, opts: &RunOpts, cancel: &Cancel) -> CaseResult {
    let t0 = now_ms();
    let analyzed = yaml::analyze_case(text);
    let Some(case) = analyzed.case.filter(|_| analyzed.valid) else {
        return skipped(file, analyzed.error.unwrap_or_else(|| "不是有效的用例".into()), t0);
    };
    if case.requests.is_empty() {
        return skipped(file, "用例没有任何请求", t0);
    }

    // 变量隔离：每个 case 一份独立上下文（见模块文档）
    let mut ctx = RunContext::new(&opts.environment.vars, case.vars.as_ref());
    let mut steps = Vec::with_capacity(case.requests.len());
    for i in topo_order(&case.requests) {
        if cancel.is_cancelled() {
            break;
        }
        let st = &case.requests[i];
        let (result, outputs) = run_step(st, &ctx, opts).await;
        ctx.steps.insert(st.id.clone(), outputs);
        let stop = opts.stop_on_failure && result.status != StepStatus::Passed;
        steps.push(result);
        if stop {
            break;
        }
    }

    CaseResult {
        file: file.to_string(),
        name: case.name.filter(|n| !n.is_empty()).unwrap_or_else(|| file_name_of(file)),
        status: rollup(&steps),
        skip_reason: None,
        started_at: iso8601(t0),
        duration_ms: now_ms().saturating_sub(t0),
        steps,
    }
}

// ── 批量 ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchTarget {
    /// 相对工作空间根（报告里记的就是它）
    pub file: String,
    /// 绝对路径（读盘用）
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchMeta {
    pub workspace: WorkspaceInfo,
    pub tool_version: String,
    pub options: RunOptions,
}

/// 进度回调。每完成一个 case 调一次，参数是当前报告的快照。
/// 节流（比如周期写盘）由调用方决定——core 不该替上层判断多久写一次盘合适。
pub type ProgressFn = Arc<dyn Fn(&RunReport) + Send + Sync>;

/// 读一个 case 文件并跑完。读盘失败记为 skipped 并给出原因，**不静默丢弃**——
/// 静默跳过会让人误以为全跑过了。
async fn run_one(t: &BatchTarget, opts: &RunOpts, cancel: &Cancel) -> CaseResult {
    // case 文件是几 KB 的文本，同步读的耗时相对一次 HTTP 往返可以忽略
    match std::fs::read_to_string(&t.path) {
        Ok(text) => run_case(&text, &t.file, opts, cancel).await,
        Err(e) => skipped(&t.file, format!("读取失败：{e}"), now_ms()),
    }
}

/// 批量执行：逐个 case 跑完，每完成一个即回调一次当前报告快照。
///
/// `concurrency > 1` 时 case 之间并发（case 内部的 step 仍按拓扑序串行）。
/// 并发是安全的，因为 case 之间不共享任何上下文；唯一的共享点是 OAuth 2.0 的
/// token 缓存，那里已按缓存键做了 in-flight 去重（见 `auth`）。
pub async fn run_batch(
    targets: Vec<BatchTarget>,
    meta: BatchMeta,
    opts: RunOpts,
    on_progress: Option<ProgressFn>,
    cancel: Cancel,
) -> RunReport {
    let t0 = now_ms();
    let mut report = RunReport {
        schema_version: REPORT_SCHEMA_VERSION,
        tool: ToolInfo { name: "apicase".into(), version: meta.tool_version },
        started_at: iso8601(t0),
        finished_at: None,
        duration_ms: 0,
        status: RunStatus::Running,
        workspace: meta.workspace,
        environment: EnvironmentInfo {
            name: opts.environment.name.clone(),
            vars: redact_vars(&opts.environment.vars, opts.redact),
        },
        options: meta.options,
        summary: RunSummary::default(),
        cases: Vec::new(),
    };
    let emit = |r: &RunReport| {
        if let Some(f) = on_progress.as_ref() {
            f(r);
        }
    };
    emit(&report);

    let mut stopped = false;
    if opts.concurrency <= 1 {
        // 串行路径独立成一支：顺序精确、`stop_on_failure` 语义干净，
        // 也省掉了 spawn 与信号量的开销。这是默认路径。
        for t in &targets {
            if cancel.is_cancelled() {
                break;
            }
            let r = run_one(t, &opts, &cancel).await;
            let stop = opts.stop_on_failure && matches!(r.status, CaseStatus::Failed | CaseStatus::Error);
            push(&mut report, r, t0);
            emit(&report);
            if stop {
                stopped = true;
                break;
            }
        }
    } else {
        stopped = run_concurrent(&targets, &opts, &cancel, &mut report, t0, &emit).await;
    }

    let end = now_ms();
    report.status = if cancel.is_cancelled() && !stopped { RunStatus::Cancelled } else { RunStatus::Done };
    report.finished_at = Some(iso8601(end));
    report.duration_ms = end.saturating_sub(t0);
    emit(&report);
    report
}

/// 并发路径：case 之间用信号量限流，结果按**完成序**追加。
///
/// 完成序而非提交序，是为了让进度真的像"实时流"——跑完一个冒一个。
/// 串行时两者相同，所以默认行为不变。
async fn run_concurrent(
    targets: &[BatchTarget],
    opts: &RunOpts,
    cancel: &Cancel,
    report: &mut RunReport,
    t0: u64,
    emit: &impl Fn(&RunReport),
) -> bool {
    let sem = Arc::new(tokio::sync::Semaphore::new(opts.concurrency as usize));
    let mut set = tokio::task::JoinSet::new();
    for t in targets {
        let (t, opts, cancel, sem) = (t.clone(), opts.clone(), cancel.clone(), sem.clone());
        set.spawn(async move {
            // 先拿令牌再看取消：取消后仍在排队的任务应当直接放弃，而不是排到了才发现
            let _permit = sem.acquire_owned().await.ok()?;
            if cancel.is_cancelled() {
                return None;
            }
            Some(run_one(&t, &opts, &cancel).await)
        });
    }

    let mut stopped = false;
    while let Some(joined) = set.join_next().await {
        // 任务 panic 不该带走整轮运行——记下来继续收剩下的
        let Ok(Some(r)) = joined else { continue };
        let stop = opts.stop_on_failure && matches!(r.status, CaseStatus::Failed | CaseStatus::Error);
        push(report, r, t0);
        emit(report);
        if stop && !stopped {
            stopped = true;
            // 让还没拿到令牌的任务直接放弃；已在飞的请求跑完它自己
            cancel.cancel();
        }
    }
    stopped
}

fn push(report: &mut RunReport, r: CaseResult, t0: u64) {
    report.cases.push(r);
    report.summary = RunSummary::of(&report.cases);
    report.duration_ms = now_ms().saturating_sub(t0);
}

#[cfg(test)]
mod tests;
