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

export interface AiStatus {
  /** 自带 CLI 的路径；null = 这个构建里没带 */
  source: string | null;
  linkState: LinkState;
  /** 已装时是软链位置；未装时是**建议**的落点 */
  link: string | null;
  /** 落点不在 PATH 里，装完还要用户自己加进去 */
  needsPathSetup: boolean;
  /** 当前工作空间有没有 apicase 的 AGENTS.md 段落 */
  agents: boolean;
}

export const EMPTY_AI_STATUS: AiStatus = {
  source: null,
  linkState: "unavailable",
  link: null,
  needsPathSetup: false,
  agents: false,
};

export function aiStatus(workspace: string): Promise<AiStatus> {
  return invoke<AiStatus>("ai_status", { workspace: workspace || null });
}

export function installCli(): Promise<AiStatus> {
  return invoke<AiStatus>("ai_install_cli");
}

export function uninstallCli(): Promise<AiStatus> {
  return invoke<AiStatus>("ai_uninstall_cli");
}

/** 返回一句可直接显示的结果说明（新建 / 追加 / 更新 / 无改动）。 */
export function writeAgents(workspace: string): Promise<string> {
  return invoke<string>("ai_write_agents", { workspace });
}

/**
 * 给 AI 客户端用的 MCP 配置。
 *
 * 用 `apicase` 而不是自带那份的绝对路径：**这段要粘进用户的客户端配置文件**，
 * 而那个文件多半也随 dotfiles 走到别的机器。命令名是稳定的，绝对路径不是。
 * 前提是 CLI 已进 PATH——所以界面上这段展示在「安装命令行工具」下方。
 */
export function mcpConfigJson(workspace: string): string {
  return JSON.stringify(
    { mcpServers: { apicase: { command: "apicase", args: ["mcp", "-w", workspace || "<工作空间路径>"] } } },
    null,
    2,
  );
}
