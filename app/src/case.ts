// case 的类型定义 + 解析/序列化的 IPC 封装。
//
// **YAML 的解析与序列化在 Rust**（`core/src/yaml/`），这里只有两样东西：
// 与 Rust serde 模型逐字段对齐的 TS 类型，以及调用后端的薄封装。
//
// 为什么不在前端留一份 js-yaml 实现：CLI（`apicase run`）必须能自己读 case，
// 所以 Rust 侧一定要有一份；再在前端留一份，就是两份实现同步维护——
// 加一个字段要改两处，漏改的那次会以"界面能打开、CLI 读不了"的形式暴露出来。
//
// 这些调用都发生在**用户操作**边界（打开文件、切视图、保存），一次 IPC 往返
// 不到 1ms，感知不到。真正在打字热路径上的只有 URL ↔ query 同步，它留在本文件末尾、
// 仍是同步的纯字符串函数。
import { invoke } from "@tauri-apps/api/core";

/** 一行键值（query / headers / 表单项通用）；enabled 默认 true；description 可选备注 */
export interface KV {
  name: string;
  value: string;
  enabled?: boolean;
  description?: string;
}

/**
 * form-data 的一项：在 KV 之上多一个类型。
 * 文件路径直接存进 `value`（不另立子键）—— 行结构与 KV 一致，
 * 禁用/备注/`${{变量}}` 替换全部复用；YAML 里 `type: file` 一眼可辨。
 */
export interface FormItem extends KV {
  type?: "text" | "file"; // 默认 text（不落盘）；file 时 value 为本地文件路径
}

/** 请求体类型；平铺到一级（借 Apifox），不做 Postman 那层 raw + 语言二级下拉 */
export type BodyType = "none" | "json" | "xml" | "text" | "form-urlencoded" | "form-data" | "binary";

export interface BodySpec {
  type: BodyType;
  json?: unknown; // type=json：结构化对象（diff 友好）
  xml?: string;
  text?: string;
  contentType?: string; // type=text | binary 可选覆盖 Content-Type
  urlencoded?: KV[];
  formData?: FormItem[];
  filePath?: string; // type=binary：以原始字节发送的文件路径（由后端读盘）
}

/** 认证方式；命名对齐 Postman / Insomnia / Bruno 的通行叫法（见 auth.ts 的显示名） */
export type AuthType = "none" | "bearer" | "basic" | "apikey" | "digest" | "oauth2";

export interface AuthSpec {
  type: AuthType;
  bearer?: { token: string };
  basic?: { username: string; password: string };
  apikey?: { key: string; value: string; in: "header" | "query" };
  digest?: { username: string; password: string }; // RFC 7616：发送时按 401 challenge 现算
  oauth2?: {
    // 仅客户端凭据模式（client_credentials）——自动化调试最常用的一支
    tokenUrl: string;
    clientId: string;
    clientSecret: string;
    scope?: string;
    clientAuth?: "header" | "body"; // 凭据放 Basic 头还是表单体
  };
}

/** HTTP 请求报文规格（单/多请求复用）；未来多协议可另立 GrpcSpec 等，并列于 Request 之下。 */
export interface HttpSpec {
  method: string;
  url: string;
  query: KV[];
  headers: KV[];
  auth: AuthSpec;
  body: BodySpec;
}

/** 一个请求的输出提取：outputs: { token: $.data.token } → { name:"token", path:"$.data.token" } */
export interface RequestOutput {
  name: string;
  path: string;
}

/** 断言操作符（借 Step CI check / Bruno assert 的收敛形） */
export type AssertOp = "eq" | "ne" | "contains" | "exists" | "notExists" | "gt" | "lt" | "matches";
export const ASSERT_OPS: AssertOp[] = ["eq", "ne", "contains", "exists", "notExists", "gt", "lt", "matches"];

/** 单条断言：target 统一挂在 `res` 下 —— `res.status` | `res.headers.<名>` | `res.body<路径>` */
export interface Assertion {
  target: string;
  op: AssertOp;
  value?: string; // exists/notExists 无需 value
}

/**
 * 一个 step（可编排的调用节点；借 Arazzo step / GHA job）。
 * 协议由 `protocol` 显式声明（当前仅 `http`），报文承载于 `request`（未来可随协议扩展）。
 */
export interface Request {
  id: string;
  protocol: string; // 协议标识：当前仅 "http"
  http: HttpSpec; // 报文（YAML 键为 `request:`；内部沿用 http 命名承载 HttpSpec）
  dependsOn: string[]; // DAG 依赖指针（借 Arazzo dependsOn / GHA needs）
  outputs: RequestOutput[]; // JSONPath 提取
  assertions: Assertion[]; // 响应断言
  docs?: string; // 该 step 的 markdown 文档（可选）
  ui?: { x: number; y: number }; // 前端属性：画布坐标；缺省时按 dependsOn 自动布局
}

/**
 * 画布坐标的**前端内部形态**：以 step id 为键的一张表，画布按 id 查坐标最顺手。
 * 落盘时分发进各 step 的 `ui:`（见 draft.ts / App.tsx 的组装），文件里不存在这张表。
 */
export type UiNodes = Record<string, { x: number; y: number }>;

/** 一个 case：统一为 steps 列表（单请求 = 长度 1，多请求 = DAG）。 */
export interface Case {
  version: string; // apicase: v0.1（带 v 前缀，故不是数字形态、落盘不必加引号）
  name?: string;
  vars?: Record<string, unknown>;
  requests: Request[]; // 对应 YAML `steps:`（内部沿用 requests 命名）
}

/**
 * 工作空间级请求设置（application.yml 的 `settings:` 键）。跟随项目走 git，团队共享。
 * 三项都作用于**单请求与 flow 执行**——两者共用同一条执行内核通道。
 */
export interface WorkspaceSettings {
  verifySsl: boolean; // SSL/TLS 证书验证；关闭后接受任何服务端证书（降安全，UI 警示）
  useCustomCa: boolean; // 是否启用自定义 CA
  caCert: string; // CA 证书文件，**相对工作空间根**的路径（绝对路径换机器就失效）
  timeoutMs: number; // 整个请求的超时上限（毫秒），0 = 不限制
}

export const DEFAULT_WS_SETTINGS: WorkspaceSettings = {
  verifySsl: true,
  useCustomCa: false,
  caCert: "",
  timeoutMs: 0,
};

/** 环境表：`{ 环境名: { 变量: 值 } }`（顺序即 application.yml 里的书写顺序） */
export type Environments = Record<string, Record<string, string>>;

// ── IPC 封装 ───────────────────────────────────────

export interface AnalyzeResult {
  valid: boolean;
  case?: Case;
  error?: string;
}

/**
 * 校验并解析 case 文本，用于「内容驱动默认视图 / 文本兜底」。
 * valid=true 仅当能解析成对象且含 `steps:` 列表；旧格式（`requests:` / `http:` 报文键）
 * 一律判为无效 → 回退纯文本编辑（不进可视化）。
 */
export function analyzeCase(text: string): Promise<AnalyzeResult> {
  return invoke<AnalyzeResult>("analyze_case", { text });
}

/** 把 case 序列化为 YAML 文本（统一写 steps 列表；单请求 = 长度 1）。 */
export function dumpCase(c: Case): Promise<string> {
  return invoke<string>("dump_case", { case: c });
}

/**
 * 一次读全 application.yml 的两块配置。
 * 合成一个命令而非两个：这两样每次都一起用，分开就是两次 IPC + 两次 YAML 解析。
 */
export function parseAppConfig(text: string): Promise<{ environment: Environments; settings: WorkspaceSettings }> {
  return invoke("parse_app_config", { text });
}

/**
 * 把可视化编辑的 environment / settings 写回 application.yml。
 * 保留原文的其它顶层键（注释不可避免地丢失）；settings 省略时不动原文该键。
 */
export function dumpAppConfig(
  baseText: string,
  environment: Environments,
  settings?: WorkspaceSettings,
): Promise<string> {
  return invoke<string>("dump_app_config", { baseText, environment, settings: settings ?? null });
}

// ── query ↔ url 同步（前端同步实现）─────────────────
//
// 这两个函数**刻意留在前端且保持同步**：URL 输入框每敲一个字符都要调它们，
// 走 IPC 会让打字变得黏手。执行侧 Rust 有等价实现（`core/src/request.rs`），
// 用途不同——那边是"发送前把 query 合进 URL"，这边是"编辑时两个控件双向同步"。
// 都是不到 20 行的纯字符串操作，两侧各有单测钉住。
//
// 全程**不做百分号编码**：`${{var}}` 里的花括号一编码就再也替换不回来了。

/** 从 url 拆出 base 与 query 数组（保留原样，含 ${{var}}）。 */
export function splitQueryFromUrl(url: string): { base: string; query: KV[] } {
  const idx = url.indexOf("?");
  if (idx < 0) return { base: url, query: [] };
  const base = url.slice(0, idx);
  const query: KV[] = [];
  for (const pair of url.slice(idx + 1).split("&")) {
    if (pair === "") continue;
    const eq = pair.indexOf("=");
    const name = eq < 0 ? pair : pair.slice(0, eq);
    const value = eq < 0 ? "" : pair.slice(eq + 1);
    query.push({ name, value, enabled: true });
  }
  return { base, query };
}

/** 把 enabled 的 query 合并回 url（覆盖 ? 之后部分；不 encode）。 */
export function mergeQueryIntoUrl(url: string, query: KV[]): string {
  const idx = url.indexOf("?");
  const base = idx >= 0 ? url.slice(0, idx) : url;
  const enabled = query.filter((kv) => kv.enabled !== false && (kv.name.trim() !== "" || kv.value.trim() !== ""));
  if (enabled.length === 0) return base;
  return base + "?" + enabled.map((kv) => `${kv.name}=${kv.value}`).join("&");
}
