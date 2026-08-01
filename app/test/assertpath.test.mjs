// 断言目标的路径补全：候选来自最近一次响应的真实结构。
//
// 求值语义必须与执行内核（core/src/assert.rs 的 actual_for）逐条对齐——
// 补全里点得到的路径，运行时就必须取得到，否则这个功能反而在误导人。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { suggestTargets, suggestValue, evalTarget, summarize } = await loadModule("src/assertPath.ts");

const resp = {
  status: 200,
  headers: [
    { key: "Content-Type", value: "application/json" },
    { key: "X-Count", value: "7" },
  ],
  body: JSON.stringify({
    code: 0,
    msg: "ok",
    data: { token: "abcdef", "user-name": "张三", nil: null },
    list: [10, { id: 2 }],
  }),
};
const html = { status: 200, headers: [], body: "<html>hi</html>" };

const labels = (input, r = resp) => suggestTargets(input, r).map((s) => s.label);
const texts = (input, r = resp) => suggestTargets(input, r).map((s) => s.text);

// ── 逐层候选 ────────────────────────────────────────

eq(labels(""), ["res"], "空输入提示 res");
eq(labels("r"), ["res"], "打了一半也认");
eq(texts("re"), ["res"], "采纳后补全成 res");
eq(labels("res."), ["status", "headers", "body"], "res. 之后是三个域");
eq(texts("res.b"), ["res.body"], "片段过滤");
eq(labels("res.zz"), [], "没有匹配就不弹面板");

eq(labels("res.headers."), ["Content-Type", "X-Count"], "响应头名来自本次响应");
eq(labels("res.headers.x"), ["X-Count"], "过滤大小写不敏感");
eq(texts("res.headers.c"), ["res.headers.Content-Type"], "含连字符的头名走点号");

eq(labels("res.body."), ["code", "msg", "data", "list"], "响应体顶层字段按原序");
eq(labels("res.body.data."), ["token", "user-name", "nil"], "逐层深入");
eq(texts("res.body.data.u"), ["res.body.data.user-name"], "连字符 key 点号可写");

// ── 数组 ───────────────────────────────────────────

eq(labels("res.body.list."), ["[0]", "[1]"], "数组给下标，不给点号 key");
eq(texts("res.body.list."), ["res.body.list[0]", "res.body.list[1]"], "下标替掉用户打的点");
eq(texts("res.body.list["), ["res.body.list[0]", "res.body.list[1]"], "手打左括号同样提示");
eq(texts("res.body.list[1"), ["res.body.list[1]"], "下标片段参与过滤");
eq(labels("res.body.list[1]"), ["id"], "闭合下标后直接提示下一层，不必先敲点");
eq(texts("res.body.list[1]"), ["res.body.list[1].id"], "此时才补点");

// ── 方括号形式 ──────────────────────────────────────

eq(texts("res.headers['con"), ["res.headers['Content-Type']"], "方括号里的候选自带引号");
eq(texts("res.body['da"), ["res.body['data']"], "响应体 key 也能走方括号");

// ── 值摘要与「还有下一层」──────────────────────────

const domains = suggestTargets("res.", resp);
eq(
  domains.map((s) => s.hint),
  ["200", "{2}", "{4}"],
  "三个域各自的值摘要：状态码 / 头数量 / 响应体字段数",
);
eq(
  domains.map((s) => s.more),
  [false, true, true],
  "status 是叶子，headers / body 可以点进去",
);
const fields = suggestTargets("res.body.data.", resp);
eq(
  fields.map((s) => s.hint),
  ['"abcdef"', '"张三"', "null"],
  "字段值直接摆在候选上——路径取不取得到，选之前就看得见",
);
ok(suggestTargets("res.body.", resp)[3].more, "list 是数组，还有下一层");
ok(!suggestTargets("res.body.", resp)[0].more, "code 是数字，到头了");

eq(summarize("x".repeat(30)), JSON.stringify("x".repeat(24) + "…"), "长字符串截断");
eq(summarize([1, 2, 3]), "[3]", "数组给长度");
eq(summarize({ a: 1 }), "{1}", "对象给字段数");

// ── 没跑过请求时不臆造结构 ──────────────────────────

const bare = (input) => suggestTargets(input).map((s) => s.label); // 不传响应 = 还没跑过请求
eq(bare(""), ["res"], "无响应也提示 res");
eq(bare("res."), ["status", "headers", "body"], "固定字段照给");
eq(bare("res.body."), [], "但不编造响应体字段");
eq(bare("res.headers."), [], "也不编造响应头");
eq(suggestTargets("res.")[0].hint, "", "无响应时不显示值摘要");

// ── 求值：与执行内核同语义 ──────────────────────────

eq(evalTarget("res.status", resp), { found: true, value: 200 }, "状态码");
eq(evalTarget("res.headers.content-type", resp).value, "application/json", "头名大小写不敏感");
eq(evalTarget("res.headers['X-Count']", resp).value, "7", "方括号形式");
eq(evalTarget("res.body.data.token", resp).value, "abcdef", "响应体路径");
eq(evalTarget("res.body.list[1].id", resp).value, 2, "数组下标");
eq(evalTarget("res.body.data['user-name']", resp).value, "张三", "含连字符的 key 两种写法都行");
eq(evalTarget("res.body.data.nil", resp), { found: true, value: null }, "取到 null ≠ 路径不存在");
ok(!evalTarget("res.body.nope", resp).found, "路径不存在");

for (const t of ["status", "header.Content-Type", "$.data.token", "$", "data.token"]) {
  ok(!evalTarget(t, resp).found, `旧写法 ${t} 判无效（硬切换，不做双认）`);
}
for (const t of ["res", "res.", "resbody", "res.bodyfoo", "res.statusx", "res.headers", "res.foo"]) {
  ok(!evalTarget(t, resp).found, `${t} 判无效`);
}

// ── 期望值建议：只在「把当前值钉下来」确实合理时出声 ────

eq(suggestValue("res.status", "eq", resp), "200", "状态码");
eq(suggestValue("res.body.data.token", "eq", resp), "abcdef", "字符串不带引号——期望值格里填的是裸值");
eq(suggestValue("res.body.code", "eq", resp), "0", "数字转文本");
eq(suggestValue("res.headers.Content-Type", "contains", resp), "application/json", "contains 也给");
eq(suggestValue("res.body.code", "gt", resp), "0", "gt 给当前值作起点");

eq(suggestValue("res.body.data.token", "exists", resp), null, "exists 本来就不填值");
eq(suggestValue("res.headers.Content-Type", "matches", resp), null, "matches 不给原文：里头的 . 会被当通配符，看着能过其实没在断言什么");
eq(suggestValue("res.body.data.nil", "eq", resp), null, "null 在比较里等同不存在，建议它没意义");
eq(suggestValue("res.body.data", "eq", resp), null, "对象：eq 会退化成整段文本比对，该配 exists");
eq(suggestValue("res.body.list", "eq", resp), null, "数组同理");
eq(suggestValue("res.body.nope", "eq", resp), null, "路径取不到时该修的是 target，不是 value");
eq(suggestValue("status", "eq", resp), null, "旧写法的目标也取不到");
eq(suggestValue("res.status", "eq"), null, "没跑过请求就没有当前值");

const odd = {
  status: 204,
  headers: [],
  body: JSON.stringify({ flag: true, long: "x".repeat(80), empty: "", zero: 0 }),
};
eq(suggestValue("res.body.flag", "eq", odd), "true", "布尔转文本");
eq(suggestValue("res.body.zero", "eq", odd), "0", "0 是有效期望值，不能被当成空值滤掉");
eq(suggestValue("res.body.long", "eq", odd), null, "超长文本不建议——钉死一大段文本又脆又难读");
eq(suggestValue("res.body.empty", "eq", odd), null, "空串不建议：placeholder 显示「当前：」很怪");

eq(evalTarget("res.body", html).value, "<html>hi</html>", "非 JSON 响应体给原文（HTML 做 contains 是真实需求）");
ok(!evalTarget("res.body.a", html).found, "但非 JSON 上的字段路径确实取不到");
eq(evalTarget("res.body", resp).value, JSON.parse(resp.body), "JSON 响应体给结构");

report();
