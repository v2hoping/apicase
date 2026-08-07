// Cookie 管理界面的纯函数：分组、搜索、编辑态的域写法与时间互转。
// jar 本身（收发、域匹配、合法性校验、持久化）在 Rust，有自己的单测与端到端测试。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { COOKIE_JAR_REL, groupByDomain, filterCookies, domainForEdit, expiryText } =
  await loadModule("src/cookies.ts");

eq(COOKIE_JAR_REL, ".apicase/cookies.yml", "jar 与报告同在 .apicase/ 下（已随它进 .gitignore）");

const item = (domain, name, extra = {}) => ({
  domain,
  path: "/",
  name,
  value: "v",
  secure: false,
  expired: false,
  hostOnly: true,
  ...extra,
});

// ── groupByDomain ──
// 列表来自后端时已按 域 → 路径 → 名 排好序，分组只需按相邻项归并
eq(groupByDomain([]).length, 0, "空列表分不出组");

const groups = groupByDomain([item("a.test", "k1"), item("a.test", "k2"), item("b.test", "k3")]);
eq(groups.length, 2, "两个域两组");
eq(groups[0].domain, "a.test", "保持原有顺序");
eq(groups[0].items.length, 2, "同域的归到一起");
eq(groups[1].items[0].name, "k3", "第二组内容正确");

// 同名域若被别的域隔开（后端未排序的极端情况）不合并——分组只做相邻归并，
// 显示成两组也好过悄悄改变顺序让人对不上号
eq(groupByDomain([item("a", "1"), item("b", "2"), item("a", "3")]).length, 3, "不跨越地合并");

// ── filterCookies ──
// 域 / 名 / 值 一起搜：「哪个 cookie 存着这个 token」和「这个站有哪些 cookie」同样常见
const all = [
  item("api.example.com", "sid", { value: "abc123" }),
  item("api.example.com", "theme", { value: "dark" }),
  item("localhost", "token", { value: "ABC999" }),
];
eq(filterCookies(all, "").length, 3, "空查询不过滤");
eq(filterCookies(all, "   ").length, 3, "纯空白同上");
eq(filterCookies(all, "example").length, 2, "按域命中");
eq(filterCookies(all, "token").length, 1, "按名命中");
eq(filterCookies(all, "abc").length, 2, "按值命中且不区分大小写");
eq(filterCookies(all, "没有这个").length, 0, "无命中返回空");

// ── domainForEdit ──
// store 里 host-only 与带 Domain 属性的域名字符串长得一样，编辑框必须把区别写出来，
// 否则编辑一次就把「子域一并生效」悄悄丢了
eq(domainForEdit(item("api.test", "k")), "api.test", "host-only 原样");
eq(domainForEdit(item("test.com", "k", { hostOnly: false })), ".test.com", "带 Domain 属性的补前导点");

// ── 过期时间 ──
// 显示格式复用 datetime.ts 的那份（列表与编辑框对不上号最容易让人以为改错了），那边另有用例
eq(expiryText(item("a", "k")), "会话", "无 expiresMs = 会话 cookie");
eq(
  expiryText(item("a", "k", { expiresMs: new Date(2026, 8, 1, 10, 30).getTime() })),
  "2026-09-01 10:30",
  "有过期时间按本地时间显示",
);

report();
