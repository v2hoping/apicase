// 报告页的报文体格式化（JSON ↔ YAML）。
//
// 这段代码跑在浏览器里、没有模块系统，故按 `// #region fmt` 标记从 report.js 里截出来求值。
// 值得单测的理由：YAML 是给人复制走的——引号少一个，粘到别处就换了意思。
import { readFileSync } from "node:fs";
import { eq, ok, report } from "./harness.mjs";

const src = readFileSync("core/src/render/report.js", "utf8");
const start = src.indexOf("// #region fmt");
const end = src.indexOf("// #endregion fmt");
ok(start >= 0 && end > start, "report.js 里能找到 #region fmt 标记（改名了就同步改这里）");

const { toYaml, asJson, format } = new Function(src.slice(start, end) + "\nreturn ApicaseFmt;")();

// ── 标量与引号 ──
eq(toYaml({ a: 1, b: true, c: null }), "a: 1\nb: true\nc: null\n", "数字 / 布尔 / 空值裸写");
eq(toYaml({ s: "hello" }), "s: hello\n", "普通字符串不加引号");
eq(toYaml({ s: "200" }), "s: '200'\n", "数字形态的字符串必须加引号，否则读回来是数字");
eq(toYaml({ s: "true" }), "s: 'true'\n", "布尔形态同理");
eq(toYaml({ s: "" }), "s: ''\n", "空串");
eq(toYaml({ s: " x" }), "s: ' x'\n", "首尾空白会被吃掉");
eq(toYaml({ s: "a: b" }), "s: 'a: b'\n", "值里有「: 」会被读成映射");
eq(toYaml({ s: "- x" }), "s: '- x'\n", "以指示符开头");
eq(toYaml({ s: "it's" }), "s: it's\n", "单引号不在开头就不是指示符，裸写即可");
eq(toYaml({ s: "'q'" }), "s: '''q'''\n", "以单引号开头才加引号，内部的写两遍转义（反斜杠因此可以原样）");
eq(toYaml({ s: "C:\\\\path\\\\d+" }), "s: C:\\\\path\\\\d+\n", "反斜杠原样，不必双写");

// ── 嵌套 ──
eq(
  toYaml({ user: { id: 7, name: "张三" } }),
  "user:\n  id: 7\n  name: 张三\n",
  "嵌套对象缩进两格",
);
eq(toYaml({ tags: ["a", "b"] }), "tags:\n  - a\n  - b\n", "数组每项一行");
eq(toYaml({ empty: {}, none: [] }), "empty: {}\nnone: []\n", "空容器写成流式，不留一个悬空的键");
eq(
  toYaml({ list: [{ id: 1, ok: true }, { id: 2, ok: false }] }),
  "list:\n  - id: 1\n    ok: true\n  - id: 2\n    ok: false\n",
  "对象数组：首个键跟在 - 后面，其余键与它左对齐",
);
eq(toYaml([1, 2]), "- 1\n- 2\n", "顶层数组");
// 键位置更宽松（同 core 的 emitter）：键恒按字符串取用，没有类型歧义
eq(toYaml({ no: 1, 123: "x" }), "123: x\nno: 1\n", "布尔 / 数字形态的键裸写，不加噪声引号");
eq(toYaml({ "a b": 1, "a: b": 2 }), "a b: 1\n'a: b': 2\n", "键里出现「: 」仍要加引号");
eq(toYaml({ m: [[1, 2]] }), "m:\n  -\n    - 1\n    - 2\n", "嵌套数组");

// ── 多行文本走块标量 ──
eq(toYaml({ t: "l1\nl2" }), "t: |-\n  l1\n  l2\n", "不以换行结尾用 |-");
eq(toYaml({ t: "l1\nl2\n" }), "t: |\n  l1\n  l2\n", "以换行结尾用 |");

// ── asJson / format ──
eq(asJson('{"a":1}').a, 1, "对象能解析");
eq(asJson("[1]").length, 1, "数组能解析");
eq(asJson("123"), undefined, "裸标量不算结构化（给它切 YAML 没有意义）");
eq(asJson("<html>"), undefined, "非 JSON");
eq(asJson(""), undefined, "空");

eq(format('{"a":1}', "pretty"), '{\n  "a": 1\n}', "pretty 是缩进两格的 JSON");
eq(format('{"a":1}', "yaml"), "a: 1\n", "yaml");
eq(format('{"a":1}', "raw"), '{"a":1}', "raw 是原文");
eq(format("not json", "yaml"), "not json", "认不出 JSON 时一律回落原文，不糊弄");

report();
