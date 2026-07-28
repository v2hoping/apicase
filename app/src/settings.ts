// 应用级设置：持久化到 Tauri 应用配置目录下的 settings.json
// （macOS: ~/Library/Application Support/com.apicase.app/settings.json）。
// 与 localStorage 的差别：只按应用 identifier 定位，与启动方式（dev / 打包 / 浏览器）无关，
// 不会像 localStorage 那样按 origin 分桶导致「dev 设的、打包后读不到」。
// 读写走后端命令 read_app_settings / write_app_settings（Rust 用 app_config_dir）。
//
// 首帧缓存：settings.json 走 IPC 是异步的，首帧拿不到值。故每次写盘时在 localStorage
// 留一份整份镜像（CACHE_KEY），供同步读取——主题尤其需要，异步解析会先渲染一帧亮色再变暗。
// 权威源始终是 settings.json：缓存只用于首帧，异步读完即以磁盘值为准并刷新缓存。
import { invoke } from "@tauri-apps/api/core";
import { type ThemeMode, DEFAULT_THEME_MODE, normalizeThemeMode } from "./theme";
import { type ProxyConfig, DEFAULT_PROXY, normalizeProxyConfig } from "./proxy";
import { type Overrides, normalizeOverrides } from "./shortcuts";

export interface AppSettings {
  recentWorkspaces: string[]; // 最近打开的工作空间（最近在前，最多 10 条）
  theme: ThemeMode; // 主题（浅色 / 深色 / 跟随系统）
  proxy: ProxyConfig; // 代理设置
  shortcuts: Overrides; // 快捷键自定义绑定
  shortcutsEnabled: boolean; // 快捷键功能总开关
}

export const DEFAULT_APP_SETTINGS: AppSettings = {
  recentWorkspaces: [],
  theme: DEFAULT_THEME_MODE,
  proxy: { ...DEFAULT_PROXY },
  shortcuts: {},
  shortcutsEnabled: true,
};

/** 首帧同步读取用的镜像（localStorage）；权威源仍是 settings.json。 */
const CACHE_KEY = "apicase.settings.cache.v1";

/**
 * 各偏好早期各自独占的 localStorage 键。settings.json 里缺对应字段时从这里回落，
 * 实现老配置的无感迁移；迁移写盘成功后即清除，不留残迹。
 */
const LEGACY_KEYS = {
  theme: "apicase.theme.v1",
  proxy: "apicase.proxy.v1",
  shortcuts: "apicase.shortcuts.v1",
  shortcutsEnabled: "apicase.shortcuts.enabled.v1",
} as const;

function isObj(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

function readJson(raw: string | null): unknown {
  if (!raw) return undefined;
  try {
    return JSON.parse(raw);
  } catch {
    return undefined;
  }
}

/** 读旧键。仅在 settings.json 尚无对应字段时作为回落值，逐项独立（老版本可能只设过其中几项）。 */
function readLegacy(): Partial<AppSettings> {
  const out: Partial<AppSettings> = {};
  try {
    const theme = localStorage.getItem(LEGACY_KEYS.theme);
    if (theme !== null) out.theme = normalizeThemeMode(theme);
    const proxy = readJson(localStorage.getItem(LEGACY_KEYS.proxy));
    if (proxy !== undefined) out.proxy = normalizeProxyConfig(proxy);
    const sc = readJson(localStorage.getItem(LEGACY_KEYS.shortcuts));
    if (sc !== undefined) out.shortcuts = normalizeOverrides(sc);
    const en = localStorage.getItem(LEGACY_KEYS.shortcutsEnabled);
    if (en !== null) out.shortcutsEnabled = en !== "0"; // 老格式："1" / "0"
  } catch {
    /* localStorage 不可用（隐私模式等）时按「没有旧值」处理 */
  }
  return out;
}

function clearLegacy(): void {
  try {
    for (const k of Object.values(LEGACY_KEYS)) localStorage.removeItem(k);
  } catch {
    /* ignore */
  }
}

/** 任意对象 → 完整 AppSettings；每个字段独立兜底，一处写坏不影响其余。 */
function parseAppSettings(v: unknown, fallback: Partial<AppSettings> = {}): AppSettings {
  const o = isObj(v) ? v : {};
  const pick = <K extends keyof AppSettings>(key: K, parse: (raw: unknown) => AppSettings[K]): AppSettings[K] =>
    o[key] !== undefined ? parse(o[key]) : ((fallback[key] as AppSettings[K]) ?? DEFAULT_APP_SETTINGS[key]);
  return {
    recentWorkspaces: Array.isArray(o.recentWorkspaces)
      ? o.recentWorkspaces.filter((x): x is string => typeof x === "string")
      : (fallback.recentWorkspaces ?? []),
    theme: pick("theme", normalizeThemeMode),
    proxy: pick("proxy", normalizeProxyConfig),
    shortcuts: pick("shortcuts", normalizeOverrides),
    shortcutsEnabled: pick("shortcutsEnabled", (raw) => raw !== false),
  };
}

function writeCache(s: AppSettings): void {
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(s));
  } catch {
    /* ignore */
  }
}

/**
 * 同步读取首帧可用的设置：优先 localStorage 镜像，其次旧键，最后默认值。
 * 只用于首帧渲染（主题防闪白、各 state 的初值），随后会被 loadAppSettings 的磁盘值覆盖。
 */
export function loadCachedSettings(): AppSettings {
  try {
    return parseAppSettings(readJson(localStorage.getItem(CACHE_KEY)), readLegacy());
  } catch {
    return { ...DEFAULT_APP_SETTINGS };
  }
}

/**
 * 读取应用设置（权威源）。文件缺失 / 解析失败 / 非 Tauri 环境一律兜底为默认，不抛错。
 * settings.json 里没有的字段回落到旧的 localStorage 键——老用户升级后设置不会丢。
 */
export async function loadAppSettings(): Promise<AppSettings> {
  const legacy = readLegacy();
  try {
    const raw = await invoke<string>("read_app_settings");
    const parsed = parseAppSettings(readJson(raw), legacy);
    writeCache(parsed);
    return parsed;
  } catch {
    const fallback = parseAppSettings(undefined, legacy);
    writeCache(fallback);
    return fallback;
  }
}

/**
 * 写回应用设置（整份覆盖）+ 刷新首帧缓存。
 * 持久化失败不应中断主流程，故吞掉错误；写盘成功即清掉旧键（迁移完成）。
 */
export async function saveAppSettings(s: AppSettings): Promise<void> {
  writeCache(s);
  try {
    await invoke("write_app_settings", { content: JSON.stringify(s, null, 2) });
    clearLegacy();
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
