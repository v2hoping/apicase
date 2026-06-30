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

export interface Case {
  version: string; // apicase: "0.1"
  name?: string;
  vars?: Record<string, unknown>;
  kind: CaseKind;
  request?: RequestSpec; // kind=single 时存在
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
    return { version, name, vars, kind: "flow" };
  }
  return { version, name, vars, kind: "single", request: normalizeRequest(o.request) };
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

/** 把单节点 case 序列化为 YAML 文本。 */
export function dumpCase(c: Case): string {
  const out: Record<string, unknown> = { apicase: c.version || "0.1" };
  if (c.name) out.name = c.name;
  if (c.vars && Object.keys(c.vars).length) out.vars = c.vars;
  if (c.request) out.request = serializeRequest(c.request);
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
