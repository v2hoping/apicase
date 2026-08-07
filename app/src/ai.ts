// 设置页「AI」分区的 IPC 封装。
//
// 判断逻辑一律在 Rust（`core::cli_link` / `core::agents`，与 `apicase self install`
// 和 `apicase init` 同一份）——前端只负责显示状态与转达用户的点击。
// 在这儿复刻一份「哪个目录能装」必然与内核漂移。

import { invoke } from "@tauri-apps/api/core";

/** 命令行工具的接入状态。字符串与 Rust 侧一一对应，改一边就静默错位。 */
export type LinkState =
  | "installed" // 已装好，且指向本应用自带的那份
  | "foreign" // PATH 里有个 apicase，但不是这一份（可能是 cargo install 装的）
  | "missing" // 没装
  | "unavailable"; // 这个构建里没带 CLI（例如绕过 tauri CLI 直接 cargo run）

/** AGENTS.md 里 apicase 段落的状态。同样与 Rust 侧一一对应。 */
export type AgentsState =
  | "ready" // 已配置，且是当前版本
  | "stale" // 段落在，但内容是旧版本的（升级后的常态）
  | "absent"; // 未配置

export interface AiStatus {
  /** 自带 CLI 的路径；null = 这个构建里没带 */
  source: string | null;
  linkState: LinkState;
  /** 已装时是软链位置；未装时是**建议**的落点 */
  link: string | null;
  /** 落点不在 PATH 里，装完还要用户自己加进去 */
  needsPathSetup: boolean;
  /** 当前工作空间里 apicase 段落的状态 */
  agentsState: AgentsState;
}

export const EMPTY_AI_STATUS: AiStatus = {
  source: null,
  linkState: "unavailable",
  link: null,
  needsPathSetup: false,
  agentsState: "absent",
};

export function aiStatus(workspace: string): Promise<AiStatus> {
  return invoke<AiStatus>("ai_status", { workspace: workspace || null });
}

export function installCli(): Promise<AiStatus> {
  return invoke<AiStatus>("ai_install_cli");
}

/** 返回一句可直接显示的结果说明（新建 / 追加 / 更新 / 无改动）。 */
export function writeAgents(workspace: string): Promise<string> {
  return invoke<string>("ai_write_agents", { workspace });
}

/** MCP server 在客户端里的名字。用短名而不是「apicase-mcp」：AI 调用时看到的是 `apicase_run` 这类工具名。 */
const MCP_NAME = "apicase";

/**
 * 一家 Agent 客户端的两种接入方式。
 *
 * 给的是**用户真正要执行 / 粘贴的那一段**，而不是 `apicase mcp -w …` 这条服务端启动命令——
 * 后者是给客户端去 spawn 的，用户自己敲它只会得到一个卡住的 stdio 进程。
 *
 * **刻意不带 `-w`**：MCP 配置是给客户端长期用的，而客户端明天可能开在别的项目上。
 * 不带它时 server 就把**调用方的工作目录**当工作空间（有没有 `application.yml` 都能用），
 * AI 还能在每次调用里传 `workspace` 换到别的项目——跟着「现在开的是哪个项目」走才是对的。
 * 写死一个路径还有第二个坏处：目录改名或换机器后 server 连起都起不来（启动即校验）。
 *
 * 一律用命令名 `apicase` 而不是自带那份 CLI 的绝对路径：这些命令与配置会写进随 dotfiles
 * 走到别的机器的文件里。命令名是稳定的，绝对路径不是（前提是 CLI 已进 PATH，
 * 所以界面上这块排在「初始化」下方）。
 *
 * **删除方式与接入方式同等重要**：装完发现不对却找不到怎么撤，比装不上更让人恼火。
 */
export interface McpClient {
  id: string;
  label: string;
  /** 命令配置；缺省 = 这家没有命令行入口（IDE 型客户端），只能走文件配置。
   *  **落点写在命令上方的 `#` 注释里**——命令行注释是可以连同命令一起复制的，
   *  另起一行说明只是把同一件事说两遍 */
  cli?: {
    add: string;
    remove: string;
  };
  /** 文件配置 */
  file: {
    /** 配置写在哪：文件路径（含 Windows 的写法）或 IDE 里的面板位置。
     *  缺省 = 不占那一行（片段本身已经说明了是哪种配置文件） */
    where?: string;
    snippet: string;
    /** 删除区要展示的内容。**缺省 = 上面那段 `snippet` 自己**——要撤销的就是它，
     *  比一句「把 xx 那一项删掉」的说明直观得多。
     *  面板型客户端（文件里没有对应片段）在这里给一句操作说明 */
    removeText?: string;
    /** 删除区里**标红的那一段**（`snippet` 的子串）。配置文件多半还有别的内容，
     *  整块标红会读成「这个文件要整份删掉」；标出 apicase 这一项才是实情。
     *  缺省 = 整段都要删 */
    removeMark?: string;
    /** 删除之外的补充选项（如「先停用」），没有则不占版面 */
    removeNote?: string;
  };
}

/** 通用 `mcpServers` 片段（Claude Code / Trae / Qoder / Cursor / Windsurf 等都读这个形状）。 */
const MCP_SERVERS_ITEM = `"${MCP_NAME}": {\n      "command": "apicase",\n      "args": ["mcp"]\n    }`;
const MCP_SERVERS_JSON = `{\n  "mcpServers": {\n    ${MCP_SERVERS_ITEM}\n  }\n}`;

// 片段一律手写而不是 JSON.stringify(_, null, 2)：后者把 args 拆成多行，
// 五行能说清的配置占掉十几行，粘贴的人还得自己数括号。
/** OpenCode 的键是 `mcp`（不是 `mcpServers`），命令写成数组 */
const OPENCODE_ITEM =
  `"${MCP_NAME}": {\n      "type": "local",\n      "command": ["apicase", "mcp"],\n      "enabled": true\n    }`;

export const MCP_CLIENTS: McpClient[] = [
  {
    id: "claude",
    label: "Claude Code",
    cli: {
      // 三种作用域都给出来：装一次处处能用（user）与只给这个项目（默认）是两种真实需求，
      // 只给一条就总有人要去翻文档。`--` 之后才是要 spawn 的命令，
      // 少了它后面的参数会被 claude 自己的解析吃掉
      add: [
        "# 仅当前项目（默认）· 存 ~/.claude.json 的该项目名下",
        `claude mcp add ${MCP_NAME} -- apicase mcp`,
        "",
        "# 所有项目 · 存 ~/.claude.json（Windows：%USERPROFILE%\\.claude.json）",
        `claude mcp add -s user ${MCP_NAME} -- apicase mcp`,
        "",
        "# 随仓库共享给团队 · 写进项目根 .mcp.json",
        `claude mcp add -s project ${MCP_NAME} -- apicase mcp`,
      ].join("\n"),
      remove: [
        "# 仅当前项目（默认）",
        `claude mcp remove ${MCP_NAME}`,
        "",
        "# 所有项目",
        `claude mcp remove ${MCP_NAME} -s user`,
        "",
        "# 随仓库共享的那份",
        `claude mcp remove ${MCP_NAME} -s project`,
      ].join("\n"),
    },
    file: {
      snippet: MCP_SERVERS_JSON,
      removeMark: MCP_SERVERS_ITEM,
    },
  },
  {
    id: "codex",
    label: "Codex",
    cli: {
      add: [
        "# 所有项目 · 存 ~/.codex/config.toml（Windows：%USERPROFILE%\\.codex\\config.toml）",
        "# 仅某个项目：命令行没有作用域选项，改用「文件配置」",
        `codex mcp add ${MCP_NAME} -- apicase mcp`,
      ].join("\n"),
      remove: ["# 所有项目", `codex mcp remove ${MCP_NAME}`].join("\n"),
    },
    file: {
      where: "所有项目 → ~/.codex/config.toml（Windows：%USERPROFILE%\\.codex\\config.toml）；仅某个项目 → 该项目的 .codex/config.toml（需是受信任目录）",
      // Codex 的配置是 TOML，不是 JSON——照 mcpServers 那套贴过去会被整份忽略
      // 整段就是 apicase 这一项，没有别的内容夹在里面
      snippet: `[mcp_servers.${MCP_NAME}]\ncommand = "apicase"\nargs = ["mcp"]`,
    },
  },
  {
    id: "opencode",
    label: "OpenCode",
    cli: {
      add: [
        "# 交互式向导：名称填 apicase、类型选 local、命令填 apicase mcp",
        "# 作用域（项目根 opencode.json / 全局 ~/.config/opencode/opencode.json）也在向导里选",
        "opencode mcp add",
      ].join("\n"),
      remove: ["# 从上面选的那个位置删掉", `opencode mcp remove ${MCP_NAME}`].join("\n"),
    },
    file: {
      where: "仅当前项目 → 项目根 opencode.json；所有项目 → ~/.config/opencode/opencode.json",
      // OpenCode 的键是 mcp（不是 mcpServers），命令是数组形式
      snippet: `{\n  "mcp": {\n    ${OPENCODE_ITEM}\n  }\n}`,
      removeMark: OPENCODE_ITEM,
      removeNote: "不想删也可以先改成 \"enabled\": false 停用。",
    },
  },
  {
    id: "trae",
    label: "Trae",
    file: {
      where: "在 Trae 的 设置 → MCP → 手动添加 里粘贴（面板里可打开原始配置文件，批量改在那儿更快）",
      snippet: MCP_SERVERS_JSON,
      removeText: "在同一面板里删除 apicase 这一项",
    },
  },
  {
    id: "qoder",
    label: "Qoder",
    file: {
      where: "在 Qoder 的 设置（macOS ⌘⇧, / Windows Ctrl+Shift+,）→ MCP → ＋ 添加 里粘贴",
      snippet: MCP_SERVERS_JSON,
      removeText: "在同一面板里删除 apicase 这一项",
    },
  },
];
