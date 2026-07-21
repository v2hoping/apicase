// 应用级设置：持久化到 Tauri 应用配置目录下的 settings.json
// （macOS: ~/Library/Application Support/com.apicase.app/settings.json）。
// 与 localStorage 的差别：只按应用 identifier 定位，与启动方式（dev / 打包 / 浏览器）无关，
// 不会像 localStorage 那样按 origin 分桶导致「dev 设的、打包后读不到」。
// 读写走后端命令 read_app_settings / write_app_settings（Rust 用 app_config_dir）。
import { invoke } from "@tauri-apps/api/core";

export interface AppSettings {
  recentWorkspaces: string[]; // 最近打开的工作空间（最近在前，最多 10 条）
}

const DEFAULTS: AppSettings = { recentWorkspaces: [] };

/** 读取应用设置；文件缺失 / 解析失败 / 非 Tauri 环境一律兜底为默认，不抛错。 */
export async function loadAppSettings(): Promise<AppSettings> {
  try {
    const raw = await invoke<string>("read_app_settings");
    if (!raw) return { ...DEFAULTS };
    const o = JSON.parse(raw) as Partial<AppSettings>;
    return {
      recentWorkspaces: Array.isArray(o?.recentWorkspaces)
        ? o.recentWorkspaces.filter((x): x is string => typeof x === "string")
        : [],
    };
  } catch {
    return { ...DEFAULTS };
  }
}

/** 写回应用设置（整份覆盖）。持久化失败不应中断主流程，故吞掉错误。 */
export async function saveAppSettings(s: AppSettings): Promise<void> {
  try {
    await invoke("write_app_settings", { content: JSON.stringify(s, null, 2) });
  } catch {
    /* ignore */
  }
}

/**
 * 单个路径是否存在。校验本身抛错时**保守返回 true**（视为存在），
 * 避免一次 IO 抖动误删记录 / 误拦打开；只有后端明确判定不存在才返回 false。
 */
export async function pathExists(p: string): Promise<boolean> {
  try {
    return await invoke<boolean>("path_exists", { path: p });
  } catch {
    return true;
  }
}

/**
 * 过滤出仍存在的路径（剔除已删除 / 移动的工作空间）。
 * 仅当后端**明确**判定不存在才剔除；校验本身抛错时保守保留（见 pathExists）。
 * 调用方将过滤结果写回 settings.json，失效项即从文件中清除（非仅显示层过滤）。
 */
export async function filterExistingPaths(paths: string[]): Promise<string[]> {
  const checks = await Promise.all(paths.map(async (p) => ((await pathExists(p)) ? p : null)));
  return checks.filter((p): p is string => p !== null);
}
