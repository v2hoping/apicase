//! 执行引擎：`run_step` / `run_case` / `run_batch` 三级。
//!
//! **调试运行与回归运行走同一份实现**——界面上点「发送」是 `run_step`，
//! 目录批量运行是 `run_batch`，中间只差报文体截断这一个开关。一份执行语义，
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
    /// 断言失败是否**不**阻断下游。默认 `false` = 阻断（`error` 不受此影响，恒阻断）。
    /// 与 `stop_on_failure` 正交：那个管"要不要继续跑后面的 case"，这个管"case 内部谁该跑"。
    #[serde(default)]
    pub continue_on_assertion_failure: bool,
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
    /// 批量运行的默认参数：串行、失败继续、报文体截断 64KB。
    pub fn for_batch(environment: EnvironmentInfo) -> Self {
        Self {
            environment,
            concurrency: 1,
            stop_on_failure: false,
            continue_on_assertion_failure: false,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            client: ClientConfig::default(),
        }
    }

    /// 调试运行的默认参数：**不截断**——响应区要看的就是完整内容，
    /// 截断只在写进报告时才有意义（单文件 HTML 会把报文体全内联）。
    pub fn for_debug(environment: EnvironmentInfo) -> Self {
        Self {
            environment,
            concurrency: 1,
            stop_on_failure: false,
            continue_on_assertion_failure: false,
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

/// 一个被上游连累、不会执行的 step。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockedStep {
    pub id: String,
    /// **根因** step id——顺着依赖链往上第一个真正失败的那个，可能隔了好几层。
    /// 指向直接上游没用：一条长链上会得到一串"上游 B 失败"、"上游 C 失败"，
    /// 而用户要找的始终是最上面那个 A。
    pub cause: String,
}

/// 一个已跑完的 step 的结果（前端回报用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepOutcome {
    pub id: String,
    pub status: StepStatus,
}

/// 从「已跑完的结果」算出该跳过谁——**判定与传播都在这里，调用方不需要知道规则**。
///
/// 调试运行的循环在前端：它把跑完的 step 状态回报上来，拿走要跳过的清单。
/// 若让前端自己判断"这一步算不算阻断源"，`blocks_downstream` 那套规则就有了第二份表达，
/// 改开关语义时必然漏掉一处。
///
/// `outcomes` 只放**真正执行过**的 step——被跳过的是本函数的产出，不必回报回来
/// （它们已在返回值里，再喂进来只会被当成已知阻断源而从结果中略去）。
pub fn blocked_from_outcomes(
    steps: &[Step],
    outcomes: &[StepOutcome],
    continue_on_assertion_failure: bool,
) -> Vec<BlockedStep> {
    let blocking: Vec<String> = outcomes
        .iter()
        .filter(|o| o.status.blocks_downstream(continue_on_assertion_failure))
        .map(|o| o.id.clone())
        .collect();
    blocked_steps(steps, &blocking)
}

/// 依赖闭包传播：给定「已阻断」的 step id，算出还有哪些 step 会被连累。
///
/// **按依赖闭包算，不按线性顺序**——DAG 里两条独立分支，一条挂了不该连累另一条，
/// 这正是 DAG 比线性列表值钱的地方。
///
/// 调试运行的循环在前端（每跑完一步要刷新界面），批量运行的在 `run_case`。
/// 判定只在这里写一份，前端经 IPC 调用——同 `topo_order`，这类边界规则不该有两份实现。
pub fn blocked_steps(steps: &[Step], blocking: &[String]) -> Vec<BlockedStep> {
    // id → 根因。失败节点自己是自己的根因；被连累的继承上游的根因
    let mut cause: HashMap<String, String> = blocking.iter().map(|id| (id.clone(), id.clone())).collect();
    let mut out = Vec::new();
    // 拓扑序保证上游先处理，传递性自然成立（A 挂 → B 跳 → C 也跳）
    for i in topo_order(steps) {
        let s = &steps[i];
        if cause.contains_key(&s.id) {
            continue; // 它本身就是阻断源
        }
        let Some(root) = s.depends_on.iter().find_map(|d| cause.get(d)).cloned() else {
            continue;
        };
        cause.insert(s.id.clone(), root.clone());
        out.push(BlockedStep { id: s.id.clone(), cause: root });
    }
    out
}

/// 从一组 step id 出发，**连同它们的上游依赖闭包**一起，按拓扑序返回下标。
///
/// `apicase run --step createOrder` 用这个：只写要跑的那一个，登录之类的上游由这里补齐。
/// 否则用户得自己把整条链列出来——而链正是 DAG 里最容易列错的东西，
/// 漏一个上游的表现是「下游拿着未解析的 `${{...}}` 字面量发出真实写请求」。
///
/// 认不出的 id 一律忽略（同 `topo_order` 对未知 `dependsOn` 的处理）；
/// 「这个 id 存不存在」该由调用方在收参数时就报出来，那里才知道怎么提示。
pub fn with_dependencies(steps: &[Step], ids: &[String]) -> Vec<usize> {
    let by_id: HashMap<&str, usize> =
        steps.iter().enumerate().map(|(i, s)| (s.id.as_str(), i)).collect();
    let mut keep = vec![false; steps.len()];
    let mut stack: Vec<usize> = ids.iter().filter_map(|id| by_id.get(id.as_str()).copied()).collect();
    while let Some(i) = stack.pop() {
        if std::mem::replace(&mut keep[i], true) {
            continue; // 已收过，顺带把依赖成环挡在外面
        }
        for dep in &steps[i].depends_on {
            if let Some(&j) = by_id.get(dep.as_str()) {
                stack.push(j);
            }
        }
    }
    topo_order(steps).into_iter().filter(|&i| keep[i]).collect()
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

/// 执行单个 step：变量透传 → 发送 → 提取 outputs → 评估断言。
///
/// 返回的第二项是 outputs，下游 step 拿它发真实请求。
pub async fn run_step(step: &Step, ctx: &RunContext, opts: &RunOpts) -> (StepResult, BTreeMap<String, Value>) {
    let t0 = now_ms();
    let clip = |b: Option<&str>| BodyRecord::clip(b, opts.max_body_bytes);

    let mut result = StepResult {
        id: step.id.clone(),
        status: StepStatus::Error,
        duration_ms: 0,
        request: None,
        response: None,
        outputs: BTreeMap::new(),
        assertions: Vec::new(),
        error: None,
        skip_reason: None,
    };

    let resolved = resolve_http(&step.http, ctx);
    match send_with_auth(&resolved, &opts.client).await {
        Ok((sent, resp)) => {
            result.request = Some(record_request(&sent, opts));
            // 输出提取与断言看的是同一个响应切面（路径语法也是同一套）
            let view = RespView { status: resp.status, headers: &resp.headers, body: &resp.body };
            let outputs = extract_outputs(&step.outputs, &view);
            let assertions = eval_assertions(&step.assertions, &view);
            result.status = if assertions.iter().all(|a| a.ok) { StepStatus::Passed } else { StepStatus::Failed };
            result.response = Some(ResponseRecord {
                status: resp.status,
                status_text: resp.status_text,
                headers: resp.headers,
                body: clip(Some(&resp.body)),
                elapsed_ms: resp.elapsed_ms,
            });
            result.outputs = outputs.clone();
            result.assertions = assertions;
            result.duration_ms = now_ms().saturating_sub(t0);
            (result, outputs)
        }
        Err(e) => {
            // 请求没发出去（或认证前置步骤失败）也要把**将要发送**的报文记进报告——
            // 否则一条 "请求失败" 的错误连打到哪个 URL 都看不出来。
            result.request = Some(record_request(&crate::request::build(&resolved), opts));
            result.error = Some(e);
            result.duration_ms = now_ms().saturating_sub(t0);
            (result, BTreeMap::new())
        }
    }
}

fn record_request(req: &crate::request::HttpRequest, opts: &RunOpts) -> RequestRecord {
    RequestRecord {
        method: req.method.clone(),
        url: req.url.clone(),
        headers: req.headers.clone(),
        body: match &req.body {
            Some(RequestBody::Text(t)) => BodyRecord::clip(Some(t), opts.max_body_bytes),
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
///
/// **skipped 不参与汇总**：case 的状态应该指向根因那一步，而不是被后面一串
/// 被连累的节点稀释。跳过必然由同一个 case 内的 error / failed 引起，
/// 那个根因节点自己会把状态顶上去。
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
    run_case_model(&case, file, opts, cancel).await
}

/// 执行一个**已解析**的 case。
///
/// `run_case` 解析完就调它。单独暴露是为了「只跑其中几个 step」这类需要先裁剪模型的场景
/// （`apicase run --step`）——否则调用方得把裁剪后的模型再序列化回 YAML 绕一圈，
/// 而那一圈会把执行结果绑在序列化的往返保真度上。
pub async fn run_case_model(case: &crate::model::Case, file: &str, opts: &RunOpts, cancel: &Cancel) -> CaseResult {
    let t0 = now_ms();
    if case.requests.is_empty() {
        return skipped(file, "用例没有任何请求", t0);
    }

    // 变量隔离：每个 case 一份独立上下文（见模块文档）
    let mut ctx = RunContext::new(&opts.environment.vars, case.vars.as_ref());
    let mut steps = Vec::with_capacity(case.requests.len());
    // step id → 根因 id。上游挂了的节点不再执行（见 `blocked_steps`）
    let mut cause: HashMap<String, String> = HashMap::new();
    for i in topo_order(&case.requests) {
        if cancel.is_cancelled() {
            break;
        }
        let st = &case.requests[i];
        // 上游被阻断 → 这一步不跑，但**仍要进报告**：少一个节点，
        // 看的人无从判断它是"跑过且通过"还是"压根没跑"
        if let Some(root) = st.depends_on.iter().find_map(|d| cause.get(d)).cloned() {
            steps.push(StepResult::skipped(&st.id, format!("上游 {root} 失败")));
            cause.insert(st.id.clone(), root); // 传递性：跳过的节点同样阻断它的下游
            continue;
        }
        let (result, outputs) = run_step(st, &ctx, opts).await;
        ctx.steps.insert(st.id.clone(), outputs);
        if result.status.blocks_downstream(opts.continue_on_assertion_failure) {
            cause.insert(st.id.clone(), st.id.clone()); // 失败节点自己是根因
        }
        let stop = opts.stop_on_failure && result.status != StepStatus::Passed;
        steps.push(result);
        if stop {
            break;
        }
    }

    CaseResult {
        file: file.to_string(),
        name: case.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| file_name_of(file)),
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
        environment: opts.environment.clone(),
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

    // 运行期间收到的 cookie 走的是节流落盘（最快 1s 一次），收尾这一下把尾巴写下去：
    // 跑完就关掉应用是常态，丢掉最后那次登录会让下一轮莫名其妙地 401
    crate::cookie::flush_all();

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
