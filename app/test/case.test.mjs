// URL ↔ query 双向同步 —— 前端仅存的一处「执行相关」同步逻辑。
//
// case 的解析与序列化已整体下沉 Rust（core/src/yaml/），由那边的单测覆盖；
// 这两个函数留在前端是因为它们在**打字热路径**上（URL 输入框每敲一个字符都调），
// 走 IPC 会让输入变黏手。Rust 侧有等价实现（core/src/request.rs）服务于发送，
// 两边各自有测试钉住行为。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { splitQueryFromUrl, mergeQueryIntoUrl } = await loadModule("src/case.ts");

// ── 拆分 ────────────────────────────────────────────

eq(splitQueryFromUrl("http://x/a"), { base: "http://x/a", query: [] }, "无 query 时原样返回");
eq(
  splitQueryFromUrl("http://x/a?b=1&c=2"),
  { base: "http://x/a", query: [{ name: "b", value: "1", enabled: true }, { name: "c", value: "2", enabled: true }] },
  "拆出两个参数",
);
eq(
  splitQueryFromUrl("http://x/a?flag"),
  { base: "http://x/a", query: [{ name: "flag", value: "", enabled: true }] },
  "无值参数的 value 是空串",
);
eq(splitQueryFromUrl("http://x/a?"), { base: "http://x/a", query: [] }, "光秃秃的问号不产生空行");
eq(
  splitQueryFromUrl("http://x/a?b=1&&c=2"),
  { base: "http://x/a", query: [{ name: "b", value: "1", enabled: true }, { name: "c", value: "2", enabled: true }] },
  "连续 & 之间的空段被跳过",
);
// 值里含 = 的情形（JWT、base64 都会）：只在第一个 = 处切
eq(splitQueryFromUrl("http://x?t=a=b=c").query, [{ name: "t", value: "a=b=c", enabled: true }], "只按第一个等号切分");

// ── 合并 ────────────────────────────────────────────

eq(mergeQueryIntoUrl("http://x/a", []), "http://x/a", "空 query 不留问号");
eq(
  mergeQueryIntoUrl("http://x/a", [{ name: "b", value: "1" }, { name: "c", value: "2" }]),
  "http://x/a?b=1&c=2",
  "合并两个参数",
);
eq(
  mergeQueryIntoUrl("http://x/a?old=1", [{ name: "b", value: "1" }]),
  "http://x/a?b=1",
  "覆盖 URL 里原有的 query（表格是真相源）",
);
eq(
  mergeQueryIntoUrl("http://x/a", [{ name: "on", value: "1" }, { name: "off", value: "2", enabled: false }]),
  "http://x/a?on=1",
  "禁用行不参与合并",
);
eq(mergeQueryIntoUrl("http://x/a", [{ name: "  ", value: "  " }]), "http://x/a", "名字与值都空的占位行不参与合并");

// ── 不做百分号编码 ──────────────────────────────────
//
// 这是刻意的：`{{var}}` 里的花括号一编码就再也替换不回来了，
// 而变量占位在 URL 里是 apicase 最常见的写法。

eq(mergeQueryIntoUrl("{{base}}/a", [{ name: "q", value: "{{kw}}" }]), "{{base}}/a?q={{kw}}", "变量占位原样保留");
eq(splitQueryFromUrl("{{base}}/a?q={{kw}}").query, [{ name: "q", value: "{{kw}}", enabled: true }], "拆分同样不解码");

// ── 往返 ────────────────────────────────────────────
//
// 输入框与参数表格是双向绑定的：拆了再合必须回到原样，
// 否则每敲一个字符 URL 都会被悄悄改写一点。

for (const url of [
  "http://x/a?b=1&c=2",
  "http://x/a",
  "{{base}}/users?id={{uid}}&full=true",
  "http://x/a?中文=值",
  "http://x/a?t=a=b",
]) {
  const { base, query } = splitQueryFromUrl(url);
  eq(mergeQueryIntoUrl(base, query), url, `往返一致：${url}`);
}

// 无值参数往返后会补上 `=`——这是合并侧统一格式的结果，语义等价
const { base, query } = splitQueryFromUrl("http://x/a?flag");
ok(mergeQueryIntoUrl(base, query) === "http://x/a?flag=", "无值参数往返后补等号（语义等价）");

report();
