// flow 执行引擎（前端）：变量透传 + JSONPath 输出提取 + 断言评估。
// 与后端解耦——后端仍只做 send_request；本模块负责编排语义。
import { KV, Assertion } from "./case";
import { ReqDraft } from "./draft";

/** 运行期变量上下文：case 级 vars + 各步已提取的 outputs */
export interface RunContext {
  vars: Record<string, unknown>;
  requests: Record<string, Record<string, unknown>>; // requestId → { outputName: value }
}

// ── {{ }} 变量替换 ─────────────────────────────────
function lookup(expr: string, ctx: RunContext): unknown {
  const m = expr.match(/^(?:requests|steps)\.([^.]+)\.outputs\.(.+)$/); // 兼容旧 steps. 前缀
  if (m) return ctx.requests[m[1]]?.[m[2]];
  let key = expr;
  if (key.startsWith("vars.")) key = key.slice(5);
  return ctx.vars[key];
}

/** 替换字符串内所有 {{ expr }}；未解析的保留字面量，便于发现问题。 */
export function resolveString(s: string, ctx: RunContext): string {
  if (!s || !s.includes("{{")) return s;
  return s.replace(/\{\{\s*([^}]+?)\s*\}\}/g, (whole, expr) => {
    const v = lookup(String(expr).trim(), ctx);
    if (v === undefined || v === null) return whole;
    return typeof v === "object" ? JSON.stringify(v) : String(v);
  });
}

function resolveKV(list: KV[], ctx: RunContext): KV[] {
  return list.map((k) => ({ ...k, name: resolveString(k.name, ctx), value: resolveString(k.value, ctx) }));
}

/** 对一个请求草稿做整体变量替换（url / query / headers / auth / body）。 */
export function resolveDraft(d: ReqDraft, ctx: RunContext): ReqDraft {
  return {
    ...d,
    url: resolveString(d.url, ctx),
    query: resolveKV(d.query, ctx),
    headers: resolveKV(d.headers, ctx),
    authBearerToken: resolveString(d.authBearerToken, ctx),
    authBasicUser: resolveString(d.authBasicUser, ctx),
    authBasicPass: resolveString(d.authBasicPass, ctx),
    authApikeyKey: resolveString(d.authApikeyKey, ctx),
    authApikeyValue: resolveString(d.authApikeyValue, ctx),
    bodyText: resolveString(d.bodyText, ctx),
    bodyContentType: resolveString(d.bodyContentType, ctx),
    bodyForm: resolveKV(d.bodyForm, ctx),
  };
}

// ── JSONPath（常用子集：$ / .key / [n] / ['key'] / ["key"]）──
export function extractJsonPath(root: unknown, path: string): unknown {
  let s = path.trim();
  if (s.startsWith("$")) s = s.slice(1);
  if (s === "") return root;
  if (/^[A-Za-z_]/.test(s)) s = "." + s; // 允许省略前导点：data.token
  const re = /\.([A-Za-z_$][\w$]*)|\[(\d+)\]|\['([^']*)'\]|\["([^"]*)"\]/g;
  let cur: unknown = root;
  let m: RegExpExecArray | null;
  let end = 0;
  while ((m = re.exec(s))) {
    if (cur === null || cur === undefined) return undefined;
    const key = m[1] ?? m[3] ?? m[4] ?? Number(m[2]);
    cur = (cur as Record<string | number, unknown>)[key as string | number];
    end = re.lastIndex;
  }
  if (end !== s.length) return undefined; // 存在未识别片段 → 视为无效路径
  return cur;
}

export function parseJsonSafe(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

/** 从响应体按 outputs 提取变量。 */
export function extractOutputs(outputs: { name: string; path: string }[], body: string): Record<string, unknown> {
  const parsed = parseJsonSafe(body);
  const out: Record<string, unknown> = {};
  for (const o of outputs) {
    if (o.name.trim()) out[o.name.trim()] = extractJsonPath(parsed, o.path);
  }
  return out;
}

// ── 断言评估 ───────────────────────────────────────
export interface AssertResult {
  target: string;
  op: string;
  ok: boolean;
  actual: string;
}

interface RespLike {
  status: number;
  headers: { key: string; value: string }[];
  body: string;
}

function looseEq(a: unknown, b: string): boolean {
  if (a === undefined || a === null) return false;
  if (String(a) === b) return true;
  const na = Number(a);
  const nb = Number(b);
  return !Number.isNaN(na) && !Number.isNaN(nb) && na === nb;
}

function compare(actual: unknown, op: string, value: string): boolean {
  switch (op) {
    case "exists":
      return actual !== undefined && actual !== null;
    case "notExists":
      return actual === undefined || actual === null;
    case "eq":
      return looseEq(actual, value);
    case "ne":
      return !looseEq(actual, value);
    case "contains":
      return actual !== undefined && actual !== null && String(actual).includes(value);
    case "gt":
      return Number(actual) > Number(value);
    case "lt":
      return Number(actual) < Number(value);
    case "matches":
      try {
        return new RegExp(value).test(String(actual));
      } catch {
        return false;
      }
    default:
      return false;
  }
}

function actualFor(target: string, resp: RespLike, body: unknown): unknown {
  const t = target.trim();
  if (t === "status") return resp.status;
  if (t.toLowerCase().startsWith("header.")) {
    const hn = t.slice(7).toLowerCase();
    return resp.headers.find((h) => h.key.toLowerCase() === hn)?.value;
  }
  return extractJsonPath(body, t);
}

/** 评估一组断言，返回逐条结果。 */
export function evalAssertions(list: Assertion[], resp: RespLike): AssertResult[] {
  const body = parseJsonSafe(resp.body);
  return list
    .filter((a) => a.target.trim() !== "")
    .map((a) => {
      const actual = actualFor(a.target, resp, body);
      const ok = compare(actual, a.op, a.value ?? "");
      const shown = actual === undefined ? "∅" : typeof actual === "object" ? JSON.stringify(actual) : String(actual);
      return { target: a.target.trim(), op: a.op, ok, actual: shown };
    });
}
