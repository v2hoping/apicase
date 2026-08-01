// 断言目标的路径补全：把「最近一次响应」变成一份可点选的候选。
//
// 语义与执行内核一一对应（core/src/assert.rs 的 `actual_for`）——目标统一挂在 `res` 下：
// `res.status` / `res.headers.<名>` / `res.body<路径>`。这里再实现一遍不是重复：
// 内核在**跑完之后**求值，而补全要在**输入时**就给出候选与当前值。两边跑同一套规则，
// 所以点着选出来的路径，运行时一定取得到——`∅` 的排查成本正是这样消掉的。

/** 补全要看的响应切面（只取用得上的三样，不把整个运行态递进来）。 */
export interface RespLite {
  status: number;
  headers: { key: string; value: string }[];
  body: string;
}

/** 一条候选。 */
export interface Suggestion {
  /** 采纳后输入框的完整内容 */
  text: string;
  /** 显示名：`status` / `data` / `[0]` / `Content-Type` */
  label: string;
  /** 当前值摘要：`"abc"` / `7` / `{4}` / `[3]`；无响应可读时为空 */
  hint: string;
  /** 还有下一层 —— 采纳后自动补 `.` 并继续提示 */
  more: boolean;
}

/** 取值结果：`found` 区分「取到 null」与「路径不存在」，与内核的 `∅` 一致。 */
export interface Found {
  found: boolean;
  value?: unknown;
}

const ROOT = "res";
const MAX_ITEMS = 100; // 候选面板不该变成第二个响应体查看器
const MAX_HINT = 24; // 字符串摘要截断长度

// 响应体的解析结果只缓存最近一条：补全在每次按键时都要求值，
// 而响应体动辄几百 KB，重复 JSON.parse 会让输入发涩。
let lastBody: string | null = null;
let lastJson: unknown;

/** 宽松解析响应体；不是 JSON 返回 undefined（响应体常是 HTML 或纯文本）。 */
function parseBody(body: string): unknown {
  if (body !== lastBody) {
    lastBody = body;
    try {
      lastJson = JSON.parse(body);
    } catch {
      lastJson = undefined;
    }
  }
  return lastJson;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** 有下一层可以点进去（空对象 / 空数组不算——点进去也没东西）。 */
function isBranch(v: unknown): boolean {
  if (Array.isArray(v)) return v.length > 0;
  return isRecord(v) && Object.keys(v).length > 0;
}

/** 值摘要：字符串带引号并截断，容器给规模，其余原样。 */
export function summarize(v: unknown): string {
  if (v === undefined) return "";
  if (v === null) return "null";
  if (Array.isArray(v)) return `[${v.length}]`;
  if (typeof v === "string") return JSON.stringify(v.length > MAX_HINT ? v.slice(0, MAX_HINT) + "…" : v);
  if (typeof v === "object") return `{${Object.keys(v as object).length}}`;
  return String(v);
}

/** 取 `res.` 之后某个域的剩余部分；域名后必须是路径边界（与内核 `domain_rest` 同规则）。 */
function domainRest(rest: string, domain: string): string | null {
  if (!rest.startsWith(domain)) return null;
  const after = rest.slice(domain.length);
  return after === "" || after.startsWith(".") || after.startsWith("[") ? after : null;
}

/** `res.headers` 之后的头名：`.Content-Type` 或 `['Content-Type']` / `["…"]`。 */
function headerName(after: string): string | null {
  if (after.startsWith(".")) {
    const name = after.slice(1).trim();
    return name || null;
  }
  if (!after.startsWith("[") || !after.endsWith("]")) return null;
  const inner = after.slice(1, -1);
  const name = unquote(inner);
  return name || null;
}

/** 剥掉成对的单/双引号；没有引号返回 null。 */
function unquote(s: string): string | null {
  const q = s[0];
  if ((q === "'" || q === '"') && s.length >= 2 && s.endsWith(q)) return s.slice(1, -1);
  return null;
}

type Seg = { key: string } | { index: number };

const IDENT_HEAD = /[A-Za-z_$]/;
const IDENT_TAIL = /[A-Za-z0-9_$-]/; // 连字符在内：JSON 的 key 带它很常见（与内核 read_ident 一致）

/** 解析 `res.body` 之后的路径（`.key` / `[n]` / `['key']`）；写法不合法返回 null。 */
function segments(path: string): Seg[] | null {
  const out: Seg[] = [];
  let i = 0;
  while (i < path.length) {
    if (path[i] === "[") {
      const close = path.indexOf("]", i);
      if (close < 0) return null;
      const inner = path.slice(i + 1, close);
      const quoted = unquote(inner);
      if (quoted !== null) out.push({ key: quoted });
      else if (/^\d+$/.test(inner)) out.push({ index: Number(inner) });
      else return null;
      i = close + 1;
      continue;
    }
    if (path[i] === ".") i++;
    else if (i !== 0) return null; // 段之间只能靠 `.` 或 `[` 分隔
    if (!IDENT_HEAD.test(path[i] ?? "")) return null;
    let j = i + 1;
    while (j < path.length && IDENT_TAIL.test(path[j])) j++;
    out.push({ key: path.slice(i, j) });
    i = j;
  }
  return out;
}

/** 按内核语义在响应上求值。无响应 / 目标无效 / 路径不存在都是 `found: false`。 */
export function evalTarget(target: string, resp?: RespLite): Found {
  const t = target.trim();
  if (!resp || !t.startsWith(ROOT + ".")) return { found: false };
  const rest = t.slice(ROOT.length + 1);

  if (rest === "status") return { found: true, value: resp.status };

  const h = domainRest(rest, "headers");
  if (h !== null) {
    const want = headerName(h)?.toLowerCase();
    if (!want) return { found: false };
    const hit = resp.headers.find((x) => x.key.toLowerCase() === want);
    return hit ? { found: true, value: hit.value } : { found: false };
  }

  const b = domainRest(rest, "body");
  if (b !== null) {
    const json = parseBody(resp.body);
    // 非 JSON 响应：`res.body` 给原文（HTML 做 contains 是真实需求），`res.body.x` 取不到
    if (json === undefined) return b === "" ? { found: true, value: resp.body } : { found: false };
    const segs = segments(b);
    if (!segs) return { found: false };
    let cur: unknown = json;
    for (const s of segs) {
      if ("key" in s) {
        if (!isRecord(cur) || !(s.key in cur)) return { found: false };
        cur = cur[s.key];
      } else {
        if (!Array.isArray(cur) || s.index >= cur.length) return { found: false };
        cur = cur[s.index];
      }
    }
    return { found: true, value: cur };
  }
  return { found: false };
}

const MAX_VALUE = 64; // 再长的期望值就是「钉死一大段文本」，读不动也改不动

/**
 * 期望值格的建议 —— target 在最近一次响应上的当前值。不该建议时返回 `null`。
 *
 * **不建议的那几种情况比建议本身重要**：一键把返回值固化成期望值，很容易做出
 * 下次必红的脆弱断言（uuid / 时间戳 / 随机 id 都是这么来的）。所以这里只在
 * "把当前值钉下来"确实是用户想要的那些场合出声，其余一律闭嘴。
 */
export function suggestValue(target: string, op: string, resp?: RespLite): string | null {
  if (op === "exists" || op === "notExists") return null; // 本来就不填值
  // 原文当正则会静默错判：`application/json` 里的 `.` 是通配符，看着能过其实没在断言什么
  if (op === "matches") return null;
  const { found, value } = evalTarget(target, resp);
  if (!found) return null; // 此时该修的是 target，不是 value
  if (value === null) return null; // null 在比较里等同「不存在」，建议它毫无意义
  // 对象 / 数组：eq 会退化成整段文本比对，又长又脆——这种目标本就该配 exists
  if (typeof value === "object") return null;
  const text = String(value);
  return text && text.length <= MAX_VALUE ? text : null;
}

/** 输入被切成「已闭合前缀」+「正在输入的片段」——候选由前者求值、由后者过滤。 */
interface Split {
  head: string;
  frag: string;
  /** 片段在方括号内（`list[` / `list[0` / `headers['con`） */
  bracket: boolean;
  /** 拼接分隔符：`.` 或空（方括号候选自带边界） */
  sep: string;
}

function splitInput(input: string): Split {
  const open = input.lastIndexOf("[");
  if (open >= 0 && input.indexOf("]", open) < 0) {
    return { head: input.slice(0, open), frag: input.slice(open + 1), bracket: true, sep: "" };
  }
  // 刚打完 `]`：整串都是已闭合前缀，直接提示下一层（省得用户先敲个点才有反应）
  if (input.endsWith("]")) return { head: input, frag: "", bracket: false, sep: "." };
  const dot = input.lastIndexOf(".");
  if (dot < 0) return { head: "", frag: input, bracket: false, sep: "" };
  return { head: input.slice(0, dot), frag: input.slice(dot + 1), bracket: false, sep: "." };
}

/** head 所指节点的成员（未过滤）。`index` 为真表示成员是数组下标。 */
interface Member {
  name: string;
  value: unknown;
  index?: boolean;
  /** 覆盖「是否还有下一层」的判断（`res` 恒有下一层，但它没有值可推） */
  more?: boolean;
}

function membersOf(head: string, resp?: RespLite): Member[] {
  if (head === "") return [{ name: ROOT, value: undefined, more: true }];
  if (head === ROOT) {
    const json = resp ? parseBody(resp.body) : undefined;
    return [
      { name: "status", value: resp?.status },
      { name: "headers", value: resp ? Object.fromEntries(resp.headers.map((h) => [h.key, h.value])) : undefined },
      { name: "body", value: resp ? (json ?? resp.body) : undefined },
    ];
  }
  if (!resp) return []; // 没跑过请求就不臆造响应结构
  if (head === ROOT + ".headers") return resp.headers.map((h) => ({ name: h.key, value: h.value }));

  const { found, value } = evalTarget(head, resp);
  if (!found) return [];
  if (Array.isArray(value)) {
    return value.slice(0, MAX_ITEMS).map((v, i) => ({ name: String(i), value: v, index: true }));
  }
  if (isRecord(value)) {
    return Object.keys(value)
      .slice(0, MAX_ITEMS)
      .map((k) => ({ name: k, value: value[k] }));
  }
  return [];
}

/**
 * 由当前输入算出候选列表。空输入给 `res`，逐层给出该层的真实成员。
 * 无候选时返回空数组（调用方据此不弹面板）。
 */
export function suggestTargets(input: string, resp?: RespLite): Suggestion[] {
  const { head, frag, bracket, sep } = splitInput(input);
  const members = membersOf(head, resp);
  if (!members.length) return [];

  // 方括号里的片段可能带前导引号（`['con`），比对时剥掉
  const needle = (bracket ? frag.replace(/^['"]/, "") : frag).toLowerCase();

  const out: Suggestion[] = [];
  for (const m of members) {
    if (needle && !m.name.toLowerCase().startsWith(needle)) continue;
    // 数组下标恒用 `[n]`；对象 key 在方括号模式下补引号，否则走点号
    const insert = m.index ? `[${m.name}]` : bracket ? `['${m.name}']` : m.name;
    const text = head + (m.index || bracket ? "" : sep) + insert;
    out.push({
      text,
      label: m.index ? `[${m.name}]` : m.name,
      hint: summarize(m.value),
      more: m.more ?? isBranch(m.value),
    });
  }
  return out;
}
