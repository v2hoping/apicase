// json.ts 单元测试：响应体的美化与语法着色切分。
// 重点在「键与字符串值的区分」和「串里的数字不被误标」——这两处错了，整屏 JSON 的颜色就是乱的。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { prettyJson, tokenizeJson, JSON_COLOR_LIMIT } = await loadModule("src/json.ts");

// 取某个 class 的全部 token 文本，便于断言
const of = (s, cls) => tokenizeJson(s).filter((t) => t.cls === cls).map((t) => t.text);
// 还原：token 拼回去必须与原文逐字相同（着色绝不能吞字符）
const joined = (s) => tokenizeJson(s).map((t) => t.text).join("");

// ── prettyJson：合法即美化，非法返回 null ──
eq(prettyJson('{"a":1}'), '{\n  "a": 1\n}', "美化紧凑 JSON");
eq(prettyJson("not json"), null, "非 JSON 返回 null");
eq(prettyJson(""), null, "空串返回 null");
eq(prettyJson("<html></html>"), null, "HTML 返回 null");
eq(prettyJson("123"), "123", "裸数值也是合法 JSON");
eq(prettyJson('"abc"'), '"abc"', "裸字符串也是合法 JSON");

// ── 键 vs 字符串值 ──
const basic = prettyJson('{"name":"张三","city":"上海"}');
eq(of(basic, "json-key"), ['"name"', '"city"'], "键被标为 json-key");
eq(of(basic, "json-str"), ['"张三"', '"上海"'], "字符串值被标为 json-str");

// ── 数值 / 布尔 / null ──
// 注意 1.2e10 经 JSON.stringify 会还原成 12000000000（JSON 的正常行为，与着色无关）
const lits = prettyJson('{"id":10241,"ok":true,"no":false,"nil":null,"pi":-3.14,"exp":1.2e10}');
eq(of(lits, "json-num"), ["10241", "true", "false", "null", "-3.14", "12000000000"], "数值/布尔/null 均为字面量");
// 直接喂 tokenizer：别的序列化器可能真的吐出指数形式，切分器本身要认
eq(of('{\n  "e": 1.2e10,\n  "n": -4.5E-3\n}', "json-num"), ["1.2e10", "-4.5E-3"], "指数形式的数值可识别");

// ── 字符串里的数字不能被误标（最容易写错的一处）──
const tricky = prettyJson('{"user":"user123","ts":"2026-08-12T05:31:00Z"}');
eq(of(tricky, "json-str"), ['"user123"', '"2026-08-12T05:31:00Z"'], "串内数字不拆出来");
eq(of(tricky, "json-num"), [], "串内数字不产生数值 token");

// ── 键名里含数字、含冒号 ──
const keyish = prettyJson('{"a1":1,"x:y":2}');
eq(of(keyish, "json-key"), ['"a1"', '"x:y"'], "键名含数字与冒号仍是键");

// ── 转义字符串 ──
const esc = prettyJson('{"q":"he said \\"hi\\"","p":"a\\\\b"}');
eq(of(esc, "json-str").length, 2, "带转义引号/反斜杠的字符串完整成段");
ok(of(esc, "json-key").length === 2, "转义串之后的键仍被正确识别");

// ── 冒号归入标点，且不吞空格 ──
const punct = tokenizeJson(prettyJson('{"a":1}'));
ok(punct.some((t) => t.cls === "json-punct" && t.text.includes(":")), "冒号标为 json-punct");

// ── 还原性：任何输入下 token 拼回都等于原文 ──
for (const src of [
  '{"a":1}',
  '{"list":[1,2,{"deep":"v"}],"s":"x"}',
  '[]',
  '{}',
  '{"empty":"","zero":0,"neg":-1}',
  '{"unicode":"\\u4e2d\\u6587","emoji":"🙂"}',
]) {
  const p = prettyJson(src);
  eq(joined(p), p, `token 还原逐字相同：${src}`);
}

// ── 数组与嵌套 ──
const nested = prettyJson('{"roles":["admin","dev"],"profile":{"age":28}}');
eq(of(nested, "json-key"), ['"roles"', '"profile"', '"age"'], "嵌套对象的键全部识别");
eq(of(nested, "json-str"), ['"admin"', '"dev"'], "数组内字符串是值不是键");

// ── 上限常量存在且合理 ──
ok(typeof JSON_COLOR_LIMIT === "number" && JSON_COLOR_LIMIT > 10000, "着色上限已定义且不至于太小");

report();
