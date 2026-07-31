// 运行相关的类型 + IPC 封装。
//
// **执行引擎在 Rust**（`core/src/runner.rs`）：变量透传、请求组装、认证、发送、
// 输出提取、断言、脱敏、报告渲染，一件不落。前端在这条链路上只做两件事——
// 把配置递下去，把结果画出来。
//
// 类型与 `core/src/report.rs` 的 serde 模型逐字段对齐（camelCase）。
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import type { Request as CaseStep } from "./case";
import type { ProxyConfig } from "./proxy";

export const DEFAULT_MAX_BODY_BYTES = 64 * 1024;

// ── 报告数据模型 ────────────────────────────────────

export interface KVPair {
  key: string;
  value: string;
}

/** 一段被记录进报告的报文体：超限即截断，但 bytes 记的始终是原始大小。 */
export interface BodyRecord {
  preview: string | null;
  bytes: number;
  truncated: boolean;
}

export interface AssertRecord {
  target: string;
  op: string;
  /** exists / notExists 无期望值，用 — 占位 */
  expected: string;
  actual: string;
  ok: boolean;
}

/**
 * **failed 与 error 必须分辨**：前者是请求发出去了但断言没过（被测服务的问题），
 * 后者是请求本身失败（网络 / TLS / 超时 / 变量解析崩了，常是环境或用例自身的问题）。
 * 两者的排查方向完全不同，混成一个状态等于丢掉最有用的那点信息。
 */
export type StepStatus = "passed" | "failed" | "error" | "running";

export interface StepResult {
  id: string;
  status: StepStatus;
  durationMs: number;
  request: {
    method: string;
    url: string;
    headers: KVPair[];
    body: BodyRecord;
  } | null;
  response: {
    status: number;
    statusText: string;
    headers: KVPair[];
    body: BodyRecord;
    elapsedMs: number;
  } | null;
  outputs: Record<string, unknown>;
  assertions: AssertRecord[];
  error?: string;
}

export type CaseStatus = "passed" | "failed" | "error" | "skipped" | "running";

export interface CaseResult {
  file: string; // 相对工作空间根
  name: string; // case 的 name，缺省用文件名
  status: CaseStatus;
  skipReason?: string; // skipped 时说明（解析失败等）
  startedAt: string;
  durationMs: number;
  steps: StepResult[];
}

export interface RunSummary {
  total: number;
  passed: number;
  failed: number;
  error: number;
  skipped: number;
  assertions: { total: number; passed: number; failed: number };
}

export interface RunOptions {
  targets: string[]; // 用户选中的目标（目录或文件，相对工作空间根）
  recursive: boolean;
  environment: string;
  concurrency: number;
  stopOnFailure: boolean;
  redact: boolean;
  maxBodyBytes: number;
}

export interface RunReport {
  schemaVersion: number;
  tool: { name: string; version: string };
  startedAt: string;
  finishedAt: string | null; // 运行中为 null
  durationMs: number;
  status: "running" | "done" | "cancelled";
  workspace: { name: string; root: string };
  environment: { name: string; vars: Record<string, string> }; // 已脱敏
  options: RunOptions;
  summary: RunSummary;
  cases: CaseResult[];
}

// ── 执行参数 ────────────────────────────────────────

export interface EnvironmentInfo {
  name: string;
  vars: Record<string, string>;
}

/**
 * 客户端级配置：代理（应用级偏好）+ 请求设置（工作空间 application.yml）。
 * 执行内核按这组值缓存 HTTP 客户端——配置不变就一直复用同一个连接池。
 */
export interface ClientConfig {
  /** url 仅 custom 模式带（见 proxy.ts 的 proxyPayload） */
  proxy?: { mode: ProxyConfig["mode"]; url?: string };
  options?: { verifySsl?: boolean; caCertPath?: string; timeoutMs?: number };
}

export interface RunOpts {
  environment: EnvironmentInfo;
  /** case 之间的并发度；1 = 串行。case 内部的 step 恒按拓扑序串行。 */
  concurrency: number;
  stopOnFailure: boolean;
  /** 报告会被转发 / 归档，故批量运行默认开；调试运行关掉——响应区要看真实内容 */
  redact: boolean;
  maxBodyBytes: number;
  client: ClientConfig;
}

/** 调试运行：**不脱敏、不截断**——响应区要看的就是真实内容。 */
export function debugRunOpts(environment: EnvironmentInfo, client: ClientConfig): RunOpts {
  return {
    environment,
    concurrency: 1,
    stopOnFailure: false,
    redact: false,
    maxBodyBytes: Number.MAX_SAFE_INTEGER,
    client,
  };
}

/** 批量运行：串行、失败继续、**脱敏开启**、报文体截断 64KB。 */
export function batchRunOpts(environment: EnvironmentInfo, client: ClientConfig): RunOpts {
  return {
    environment,
    concurrency: 1,
    stopOnFailure: false,
    redact: true,
    maxBodyBytes: DEFAULT_MAX_BODY_BYTES,
    client,
  };
}

export interface BatchTarget {
  file: string; // 相对工作空间根（报告里记的就是它）
  path: string; // 绝对路径（读盘用）
}

export interface BatchMeta {
  workspace: { name: string; root: string };
  toolVersion: string;
  options: RunOptions;
}

// ── 调试运行 ────────────────────────────────────────

/** 运行期变量上下文：case 级 vars + 各步已提取的 outputs。 */
export interface RunContext {
  vars: Record<string, unknown>;
  /** stepId → { outputName: value } */
  steps: Record<string, Record<string, unknown>>;
}

export interface StepRun {
  step: StepResult;
  /** **未脱敏**的输出，供同一个 case 内透传给下游 step */
  outputs: Record<string, unknown>;
}

/**
 * 执行单个 step —— 界面上的「发送 / ▶ 运行」走这里。
 *
 * 上下文由前端传入：它是「当前这个标签页的运行态」，生命周期跟着 UI 走
 * （切标签、改用例都会重置），放后端反而要同步两份状态。
 */
export function runStep(step: CaseStep, ctx: RunContext, opts: RunOpts): Promise<StepRun> {
  return invoke<StepRun>("run_step", { step, vars: ctx.vars, steps: ctx.steps, opts });
}

/**
 * 按 dependsOn 排出可依次执行的顺序（返回下标）。
 *
 * 「运行全部」要逐步刷新界面，所以由前端驱动循环；但**排序规则只有 Rust 一份**——
 * 成环时怎么兜底、依赖指向不存在的 step 怎么处理，这些边界不该在两处各写一遍。
 */
export function topoOrder(steps: CaseStep[]): Promise<number[]> {
  return invoke<number[]>("topo_order", { steps });
}

// ── 批量运行 ────────────────────────────────────────

/**
 * 运行进度事件（**增量**推送）。
 *
 * 一份跑了 200 个用例的报告可达数 MB，每完成一个就把整份过一次 IPC，
 * 光是编解码就能把界面拖卡。故 `start` 给报告头（cases 为空），之后逐个追加 case。
 */
export type RunEvent =
  | { kind: "start"; report: RunReport; total: number }
  | { kind: "case"; case: CaseResult; summary: RunSummary; durationMs: number }
  | { kind: "end"; status: RunReport["status"]; finishedAt: string | null; summary: RunSummary; durationMs: number };

/** 订阅一次运行的进度。返回取消订阅函数。 */
export function listenRun(runId: string, on: (e: RunEvent) => void): Promise<UnlistenFn> {
  return listen<RunEvent>(`run://progress/${runId}`, (e) => on(e.payload));
}

export interface BatchArgs {
  runId: string;
  targets: BatchTarget[];
  meta: BatchMeta;
  opts: RunOpts;
  /** 给了就周期落盘（运行中可用浏览器打开看部分结果）；目录由后端创建 */
  reportFile?: string;
}

/** 启动一次批量运行，跑完返回最终报告；过程中经 `listenRun` 推进度。 */
export function runBatch(args: BatchArgs): Promise<RunReport> {
  return invoke<RunReport>("run_batch", { args });
}

/** 取消一次运行（在 case 边界生效；已发出的 HTTP 不中断）。 */
export function cancelRun(runId: string): Promise<void> {
  return invoke("cancel_run", { runId });
}

// ── 报告 ────────────────────────────────────────────

/** 报告空壳（内嵌 iframe 用；与落盘报告共用同一套渲染实现）。 */
export function reportShell(): Promise<string> {
  return invoke<string>("report_shell");
}

/**
 * 从报告 HTML 读回结构化数据（点开历史 report.html 时用）。
 * 认不出返回 null，调用方据此降级为普通文本视图——**只认自己生成的报告**。
 */
export function parseReport(text: string): Promise<RunReport | null> {
  return invoke<RunReport | null>("parse_report", { text });
}
