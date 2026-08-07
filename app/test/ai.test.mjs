// 设置页「AI」分区给出的 MCP 接入方式（每家 Agent 的命令接入 + 配置接入）。
//
// 这些是**用户直接复制去执行 / 粘进配置文件**的，拼错了就是硬失败，
// 而且失败现场在别的程序里（客户端说「server 起不来」），最难往回追。
import { loadModule, eq, ok, has, hasnt, report } from "./harness.mjs";

const { MCP_CLIENTS } = await loadModule("src/ai.ts");

const byId = Object.fromEntries(MCP_CLIENTS.map((c) => [c.id, c]));

eq(Object.keys(byId), ["claude", "codex", "opencode", "trae", "qoder"], "覆盖的 Agent 与顺序");

// ── 命令接入 ──
//
// `--` 之后才是要 spawn 的命令。少了它，后面的参数会被客户端自己的解析吃掉。
// Claude Code 的三种作用域都要给：装一次处处能用（user）与只给这个项目（默认）
// 是两种真实需求，只给一条就总有人要去翻文档
const claudeAdd = byId.claude.cli.add;
has(claudeAdd, "claude mcp add apicase -- apicase mcp", "Claude Code · 仅当前项目（默认）");
has(claudeAdd, "claude mcp add -s user apicase -- apicase mcp", "Claude Code · 所有项目");
has(claudeAdd, "claude mcp add -s project apicase -- apicase mcp", "Claude Code · 随仓库共享");
// 删除与接入一一对称：作用域对不上会删到另一处的同名条目
const claudeRemove = byId.claude.cli.remove;
for (const scope of ["", " -s user", " -s project"]) {
  has(claudeRemove, `claude mcp remove apicase${scope}`, `Claude Code 的删除覆盖${scope || " 默认"}`);
}
eq(
  claudeAdd.split("\n").filter((l) => l.startsWith("claude ")).length,
  claudeRemove.split("\n").filter((l) => l.startsWith("claude ")).length,
  "接入与删除的条数对称",
);
// 每条命令上方都有一行注释说明落点——否则三条并排根本分不出差别
for (const text of [claudeAdd, claudeRemove]) {
  const lines = text.split("\n").filter(Boolean);
  ok(lines.length % 2 === 0 && lines.every((l, i) => (i % 2 === 0 ? l.startsWith("#") : !l.startsWith("#"))),
     `每条命令都要带注释：\n${text}`);
}

has(byId.codex.cli.add, "codex mcp add apicase -- apicase mcp", "Codex 接入");
has(byId.codex.cli.remove, "codex mcp remove apicase", "Codex 删除");
// OpenCode 的 mcp add 目前只有交互式向导，硬拼一条带参数的命令会让人对着报错发懵
has(byId.opencode.cli.add, "opencode mcp add", "OpenCode 是交互式向导");
ok(byId.opencode.cli.add.includes("向导"), "OpenCode 要说明这条是向导");

// IDE 型客户端没有命令行入口——不能编一条出来
ok(!byId.trae.cli && !byId.qoder.cli, "Trae / Qoder 没有命令接入");

for (const c of MCP_CLIENTS.filter((c) => c.cli)) {
  has(c.cli.remove, "apicase", `${c.label} 的删除命令指名 apicase`);
  has(c.cli.remove, "remove", `${c.label} 的删除命令是 remove`);
  // 落点写在命令上方的注释里（界面上没有另一行说明），少了它就只剩一条光秃秃的命令
  ok(c.cli.add.split("\n").some((l) => l.startsWith("#")), `${c.label} 的命令要带注释说明落点`);
}
// 落点也要给 Windows 的写法（`~` 在那儿不成立）
for (const id of ["claude", "codex"]) {
  has(byId[id].cli.add, "%USERPROFILE%", `${byId[id].label} 的命令注释要给 Windows 路径`);
}

// ── 不写死工作空间 ──
//
// MCP 配置是给客户端长期用的，而客户端明天可能开在别的项目上。带上 `-w` 会把 server
// 钉死在配置那一刻的目录（目录改名 / 换机器后还会连启动都失败）。
for (const c of MCP_CLIENTS) {
  const both = [c.cli?.add ?? "", c.file.snippet].join("\n");
  hasnt(both, "-w", `${c.label} 的接入不该写死工作空间`);
  hasnt(both, "/Applications/", `${c.label} 不该出现本机安装路径`);
}

// ── 文件配置 ──
//
// 删除区展示的就是**要拿掉的那一段**（界面把它标成红色），所以文件型客户端不必再写说明；
// 面板型客户端文件里没有这一段，必须给一句操作说明，否则删除区就是空的
for (const c of MCP_CLIENTS) {
  if (c.cli) ok(!c.file.removeText, `${c.label} 的删除区用片段本身即可`);
  else has(c.file.removeText ?? "", "apicase", `${c.label} 要说清在面板里怎么删`);
  // 标红那一段必须真的在片段里，否则高亮静默失效（界面上看不出来，只是没红）
  if (c.file.removeMark) {
    ok(c.file.snippet.includes(c.file.removeMark), `${c.label} 的 removeMark 要能在片段里找到`);
    // 标的是 apicase 这一项，不是整个文件——外层的 mcpServers / mcp 是上下文
    ok(!c.file.removeMark.includes("mcpServers") && !c.file.removeMark.startsWith("{"),
       `${c.label} 只该标 apicase 那一项：${c.file.removeMark}`);
  }
}
// 没有命令行入口的客户端必须说清去哪儿配——那是它唯一的入口，缺了就无从下手
for (const c of MCP_CLIENTS.filter((c) => !c.cli)) {
  ok(c.file.where && c.file.where.length > 0, `${c.label} 要说清在哪个面板里配`);
}
// 给出路径的地方都要带 Windows 的写法（`~` 在那儿不成立）
has(byId.codex.file.where, "%USERPROFILE%", "Codex 的文件落点要给 Windows 路径");

// 通用 mcpServers 形状（Claude Code / Trae / Qoder）
for (const id of ["claude", "trae", "qoder"]) {
  const cfg = JSON.parse(byId[id].file.snippet);
  eq(cfg.mcpServers.apicase, { command: "apicase", args: ["mcp"] }, `${byId[id].label} 的 mcpServers 片段`);
}

// Codex 是 TOML，不是 JSON——照 mcpServers 那套贴过去会被整份忽略
const codex = byId.codex.file.snippet;
has(codex, "[mcp_servers.apicase]", "Codex 用 TOML 的表头");
has(codex, 'command = "apicase"', "Codex 的 command");
hasnt(codex, "mcpServers", "Codex 不用 mcpServers");

// OpenCode 的键是 mcp（不是 mcpServers），命令是数组
const oc = JSON.parse(byId.opencode.file.snippet);
eq(oc.mcp.apicase, { type: "local", command: ["apicase", "mcp"], enabled: true }, "OpenCode 片段");

report();
