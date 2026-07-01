// case 的 YAML schema 类型 + 解析/序列化。
// 设计见 docs/0.latest/1.产品概念模型.md 与 docs/1.feature/20260630-case读写与格式/技术方案.md。
// 本阶段在前端用 js-yaml 解析（后端只做通用文件读写）；request 结构在单/多节点复用。
import { load, dump } from "js-yaml";

/** 一行键值（query / headers / 表单项通用）；enabled 默认 true，为 true 时不落盘 */
export interface KV {
  name: string;
  value: string;
  enabled?: boolean;
}

export type BodyType = "none" | "json" | "text" | "form-urlencoded" | "form-data";

export interface BodySpec {
  type: BodyType;
  json?: unknown; // type=json：结构化对象（diff 友好）
  text?: string; // type=text
  contentType?: string; // type=text 可选覆盖 Content-Type
  urlencoded?: KV[]; // type=form-urlencoded
  formData?: KV[]; // type=form-data（本次仅 text 字段，file 后续）
}

export type AuthType = "none" | "bearer" | "basic" | "apikey";

export interface AuthSpec {
  type: AuthType;
  bearer?: { token: string };
  basic?: { username: string; password: string };
  apikey?: { key: string; value: string; in: "header" | "query" };
}

export interface RequestSpec {
  method: string;
  url: string;
  query: KV[];
  headers: KV[];
  auth: AuthSpec;
  body: BodySpec;
}

export type CaseKind = "single" | "flow";

/** 一个 step 的输出提取：outputs: { token: $.data.token } → { name:"token", path:"$.data.token" } */
export interface StepOutput {
  name: string;
  path: string;
}

/** 断言操作符（借 Step CI check / Bruno assert 的收敛形） */
export type AssertOp = "eq" | "ne" | "contains" | "exists" | "notExists" | "gt" | "lt" | "matches";
export const ASSERT_OPS: AssertOp[] = ["eq", "ne", "contains", "exists", "notExists", "gt", "lt", "matches"];

/** 单条断言：target 为 `status` | `header.<名>` | JSONPath（如 $.data.token） */
export interface Assertion {
  target: string;
  op: AssertOp;
  value?: string; // exists/notExists 无需 value
}

/** flow 中的一个节点（借 Arazzo step / GHA job）；request 结构与单节点完全复用 */
export interface Step {
  id: string;
  request: RequestSpec;
  dependsOn: string[]; // DAG 依赖指针（借 Arazzo dependsOn / GHA needs）
  outputs: StepOutput[]; // JSONPath 提取
  assertions: Assertion[]; // 响应断言
}

/** 画布坐标（与语义分离，规避 diff 噪声）；缺省时按 dependsOn 自动布局 */
export type UiNodes = Record<string, { x: number; y: number }>;

export interface Case {
  version: string; // apicase: "0.1"
  name?: string;
  vars?: Record<string, unknown>;
  kind: CaseKind;
  request?: RequestSpec; // kind=single 时存在
  assertions?: Assertion[]; // kind=single 可选：顶层响应断言
  steps?: Step[]; // kind=flow 时存在
  ui?: { nodes: UiNodes }; // kind=flow 可选：画布坐标
}

// ── 工具 ───────────────────────────────────────────
function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
const str = (v: unknown): string => (v == null ? "" : String(v));

// ── 解析（YAML → 规范化内存模型）─────────────────────
function normalizeKV(list: unknown): KV[] {
  if (!Array.isArray(list)) return [];
  return list.filter(isPlainObject).map((it) => ({
    name: str(it.name),
    value: str(it.value),
    enabled: it.enabled === false ? false : true,
  }));
}

function normalizeAuth(a: unknown): AuthSpec {
  if (!isPlainObject(a) || typeof a.type !== "string") return { type: "none" };
  const type = a.type as AuthType;
  if (type === "bearer") {
    const b = a.bearer as Record<string, unknown> | undefined;
    return { type, bearer: { token: str(b?.token) } };
  }
  if (type === "basic") {
    const b = a.basic as Record<string, unknown> | undefined;
    return { type, basic: { username: str(b?.username), password: str(b?.password) } };
  }
  if (type === "apikey") {
    const k = a.apikey as Record<string, unknown> | undefined;
    return {
      type,
      apikey: { key: str(k?.key), value: str(k?.value), in: k?.in === "query" ? "query" : "header" },
    };
  }
  return { type: "none" };
}

function normalizeBody(b: unknown): BodySpec {
  if (!isPlainObject(b) || typeof b.type !== "string") return { type: "none" };
  const type = b.type as BodyType;
  if (type === "json") return { type, json: b.json };
  if (type === "text") {
    return { type, text: str(b.text), contentType: typeof b.contentType === "string" ? b.contentType : undefined };
  }
  if (type === "form-urlencoded") return { type, urlencoded: normalizeKV(b.urlencoded) };
  if (type === "form-data") return { type, formData: normalizeKV(b.formData) };
  return { type: "none" };
}

function normalizeRequest(r: unknown): RequestSpec {
  const o = isPlainObject(r) ? r : {};
  return {
    method: (str(o.method) || "GET").toUpperCase(),
    url: str(o.url),
    query: normalizeKV(o.query),
    headers: normalizeKV(o.headers),
    auth: normalizeAuth(o.auth),
    body: normalizeBody(o.body),
  };
}

function normalizeOutputs(o: unknown): StepOutput[] {
  if (!isPlainObject(o)) return [];
  return Object.entries(o).map(([name, path]) => ({ name, path: str(path) }));
}

function normalizeAssertions(list: unknown): Assertion[] {
  if (!Array.isArray(list)) return [];
  return list
    .filter(isPlainObject)
    .map((it) => ({
      target: str(it.target),
      op: (ASSERT_OPS.includes(str(it.op) as AssertOp) ? (str(it.op) as AssertOp) : "eq") as AssertOp,
      value: it.value === undefined || it.value === null ? undefined : str(it.value),
    }))
    .filter((a) => a.target !== "");
}

function normalizeStep(s: unknown, i: number): Step {
  const o = isPlainObject(s) ? s : {};
  const id = str(o.id) || `step${i + 1}`;
  const dependsOn = Array.isArray(o.dependsOn) ? o.dependsOn.map(str).filter(Boolean) : [];
  return {
    id,
    request: normalizeRequest(o.request),
    dependsOn,
    outputs: normalizeOutputs(o.outputs),
    assertions: normalizeAssertions(o.assertions),
  };
}

function normalizeUi(u: unknown): { nodes: UiNodes } | undefined {
  if (!isPlainObject(u) || !isPlainObject(u.nodes)) return undefined;
  const nodes: UiNodes = {};
  for (const [k, v] of Object.entries(u.nodes)) {
    if (isPlainObject(v) && typeof v.x === "number" && typeof v.y === "number") {
      nodes[k] = { x: v.x, y: v.y };
    }
  }
  return Object.keys(nodes).length ? { nodes } : undefined;
}

/** 解析 case 文本。判别：有 steps → flow；否则按单节点 request。 */
export function parseCase(text: string): Case {
  let obj: unknown;
  try {
    obj = load(text) ?? {};
  } catch (e) {
    throw new Error(`YAML 解析失败: ${e instanceof Error ? e.message : String(e)}`);
  }
  const o = isPlainObject(obj) ? obj : {};
  const version = typeof o.apicase === "string" ? o.apicase : "0.1";
  const name = typeof o.name === "string" ? o.name : undefined;
  const vars = isPlainObject(o.vars) ? o.vars : undefined;
  if (Array.isArray(o.steps)) {
    return { version, name, vars, kind: "flow", steps: o.steps.map(normalizeStep), ui: normalizeUi(o.ui) };
  }
  return { version, name, vars, kind: "single", request: normalizeRequest(o.request), assertions: normalizeAssertions(o.assertions) };
}

/**
 * 校验并解析 case 文本，用于「内容驱动默认视图 / 文本兜底」。
 * valid=true 仅当能 parse 成对象且含 `request` 或 `steps`；否则回退纯文本编辑。
 */
export function analyzeCase(text: string): { valid: boolean; case?: Case; error?: string } {
  let obj: unknown;
  try {
    obj = load(text) ?? {};
  } catch (e) {
    return { valid: false, error: `YAML 解析失败：${e instanceof Error ? e.message : String(e)}` };
  }
  if (!isPlainObject(obj)) return { valid: false, error: "顶层不是对象，不是有效的 case" };
  const hasSteps = Array.isArray(obj.steps);
  const hasReq = isPlainObject(obj.request);
  if (!hasSteps && !hasReq) return { valid: false, error: "缺少 request 或 steps 字段" };
  return { valid: true, case: parseCase(text) };
}

/** 从 application.yml 文本解析 environment：`{ 环境名: { 变量: 值 } }`（值统一转字符串）。 */
export function parseEnvironments(text: string): Record<string, Record<string, string>> {
  let obj: unknown;
  try {
    obj = load(text) ?? {};
  } catch {
    return {};
  }
  if (!isPlainObject(obj) || !isPlainObject(obj.environment)) return {};
  const out: Record<string, Record<string, string>> = {};
  for (const [env, vars] of Object.entries(obj.environment)) {
    const m: Record<string, string> = {};
    if (isPlainObject(vars)) {
      for (const [k, v] of Object.entries(vars)) m[k] = v == null ? "" : typeof v === "object" ? JSON.stringify(v) : String(v);
    }
    out[env] = m;
  }
  return out;
}

/** 把可视化编辑的 environment 写回 application.yml（保留其它顶层键，注释不可避免地丢失）。 */
export function dumpApplicationConfig(baseText: string, environment: Record<string, Record<string, string>>): string {
  let obj: unknown;
  try {
    obj = load(baseText);
  } catch {
    obj = {};
  }
  const base: Record<string, unknown> = isPlainObject(obj) ? { ...obj } : {};
  base.environment = environment;
  return dump(base, { lineWidth: 100, noRefs: true });
}

// ── 序列化（内存模型 → YAML，裁剪默认值）──────────────
function serializeKV(list: KV[]): Array<Record<string, unknown>> {
  return (list || [])
    .filter((kv) => kv.name.trim() !== "" || kv.value.trim() !== "")
    .map((kv) =>
      kv.enabled === false
        ? { name: kv.name, value: kv.value, enabled: false }
        : { name: kv.name, value: kv.value },
    );
}

function serializeAuth(a: AuthSpec): Record<string, unknown> | null {
  if (!a || a.type === "none") return null;
  if (a.type === "bearer") return { type: "bearer", bearer: { token: a.bearer?.token || "" } };
  if (a.type === "basic")
    return { type: "basic", basic: { username: a.basic?.username || "", password: a.basic?.password || "" } };
  if (a.type === "apikey")
    return {
      type: "apikey",
      apikey: { key: a.apikey?.key || "", value: a.apikey?.value || "", in: a.apikey?.in || "header" },
    };
  return null;
}

function serializeBody(b: BodySpec): Record<string, unknown> | null {
  if (!b || b.type === "none") return null;
  if (b.type === "json") {
    if (b.json === undefined || b.json === null || b.json === "") return null;
    return { type: "json", json: b.json };
  }
  if (b.type === "text") {
    if (!b.text) return null;
    return b.contentType
      ? { type: "text", contentType: b.contentType, text: b.text }
      : { type: "text", text: b.text };
  }
  if (b.type === "form-urlencoded") {
    const u = serializeKV(b.urlencoded || []);
    return u.length ? { type: "form-urlencoded", urlencoded: u } : null;
  }
  if (b.type === "form-data") {
    const f = serializeKV(b.formData || []);
    return f.length ? { type: "form-data", formData: f } : null;
  }
  return null;
}

function serializeRequest(r: RequestSpec): Record<string, unknown> {
  const out: Record<string, unknown> = { method: r.method, url: r.url };
  const q = serializeKV(r.query);
  if (q.length) out.query = q;
  const h = serializeKV(r.headers);
  if (h.length) out.headers = h;
  const auth = serializeAuth(r.auth);
  if (auth) out.auth = auth;
  const body = serializeBody(r.body);
  if (body) out.body = body;
  return out;
}

function serializeAssertions(list: Assertion[]): Array<Record<string, unknown>> {
  return list
    .filter((a) => a.target.trim() !== "")
    .map((a) => {
      const o: Record<string, unknown> = { target: a.target, op: a.op };
      if (a.op !== "exists" && a.op !== "notExists" && a.value !== undefined && a.value !== "") o.value = a.value;
      return o;
    });
}

// 顺序对齐文档示例：id → dependsOn → request → outputs → assertions
function serializeStep(s: Step): Record<string, unknown> {
  const out: Record<string, unknown> = { id: s.id };
  if (s.dependsOn.length) out.dependsOn = s.dependsOn;
  out.request = serializeRequest(s.request);
  const o: Record<string, string> = {};
  for (const it of s.outputs) if (it.name.trim()) o[it.name.trim()] = it.path;
  if (Object.keys(o).length) out.outputs = o;
  const asserts = serializeAssertions(s.assertions);
  if (asserts.length) out.assertions = asserts;
  return out;
}

/** 把 case 序列化为 YAML 文本（单节点写顶层 request，多节点写 steps + ui）。 */
export function dumpCase(c: Case): string {
  const out: Record<string, unknown> = { apicase: c.version || "0.1" };
  if (c.name) out.name = c.name;
  if (c.vars && Object.keys(c.vars).length) out.vars = c.vars;
  if (c.kind === "flow" && c.steps) {
    out.steps = c.steps.map(serializeStep);
    if (c.ui && Object.keys(c.ui.nodes).length) out.ui = { nodes: c.ui.nodes };
  } else if (c.request) {
    out.request = serializeRequest(c.request);
    if (c.assertions && c.assertions.length) out.assertions = serializeAssertions(c.assertions);
  }
  return dump(out, { lineWidth: 100, noRefs: true });
}

// ── query ↔ url 同步（不做 encode，避免破坏 {{var}}）──
/** 从 url 拆出 base 与 query 数组（保留原样，含 {{var}}）。 */
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
