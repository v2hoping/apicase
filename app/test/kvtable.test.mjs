// 键值表 / 断言表的「空白尾行」规则。
//
// 这条规则曾出过 bug：空行只在编辑时追加，于是保存过的 case 再打开——行行填满、
// 末尾没有空行——就再也加不了下一行。故空行必须由渲染前的这两个纯函数保证，
// 且**不能**改动已有行（否则会把用户数据顺序搅乱）。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { kvRowsWithBlank, assertRowsWithBlank } = await loadModule("src/RequestEditor.tsx");

const blank = { name: "", value: "", enabled: true };
const p1 = { name: "a", value: "1", enabled: true };
const p2 = { name: "b", value: "2", enabled: true };

// ── 键值表 ──────────────────────────────────────────

eq(kvRowsWithBlank([]), [blank], "空表给一行空白");
eq(kvRowsWithBlank([p1, p2]), [p1, p2, blank], "两行都填满 → 补第三行（本 bug 的原始场景）");
eq(kvRowsWithBlank([p1, blank]), [p1, blank], "末行已空则不再补，避免越滚越多");
eq(kvRowsWithBlank([{ name: "", value: "v", enabled: true }]), [{ name: "", value: "v", enabled: true }, blank], "只填了值也算有内容");
eq(
  kvRowsWithBlank([{ name: "", value: "", enabled: true, description: "备注" }]),
  [{ name: "", value: "", enabled: true, description: "备注" }, blank],
  "只填了描述也算有内容",
);
eq(
  kvRowsWithBlank([{ name: "", value: "", enabled: true, type: "file" }]),
  [{ name: "", value: "", enabled: true, type: "file" }, blank],
  "form-data 选了「文件」但还没选文件，也算有内容",
);
eq(kvRowsWithBlank([{ name: "a", value: "1", enabled: false }]), [{ name: "a", value: "1", enabled: false }, blank], "停用行照样占位");

const src = [p1, p2];
kvRowsWithBlank(src);
eq(src, [p1, p2], "不改动入参（补空行是渲染态，不写回数据）");

// ── 断言表 ──────────────────────────────────────────

const a1 = { target: "status", op: "eq", value: "200" };
const a2 = { target: "$.data.token", op: "exists" };
const blankA = { target: "", op: "eq", value: "" };

eq(assertRowsWithBlank([]), [blankA], "空断言表给一行空白");
eq(assertRowsWithBlank([a1, a2]), [a1, a2, blankA], "断言填满 → 补新行");
eq(assertRowsWithBlank([a1, blankA]), [a1, blankA], "末行已空则不再补");
ok(assertRowsWithBlank([{ target: "", op: "eq", value: "200" }]).length === 2, "只填了期望值也算有内容");

report();
