// 主题（浅色 / 深色 / 跟随系统）：解析出的实际明暗写到 documentElement 的 data-theme，
// 供 CSS 的 :root[data-theme=dark] 覆盖变量。
// 持久化本身不在这里——统一由 settings.ts 写进应用配置目录的 settings.json（见该文件说明）。
export type ThemeMode = "light" | "dark" | "system";

export const DEFAULT_THEME_MODE: ThemeMode = "system";

/** 任意来源（JSON / 旧 localStorage 值）→ 合法的主题模式；不认识的一律回默认。 */
export function normalizeThemeMode(v: unknown): ThemeMode {
  return v === "light" || v === "dark" || v === "system" ? v : DEFAULT_THEME_MODE;
}

export function systemIsDark(): boolean {
  return typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** 把主题模式解析为实际明暗。 */
export function resolveTheme(mode: ThemeMode): "light" | "dark" {
  return mode === "system" ? (systemIsDark() ? "dark" : "light") : mode;
}

/** 应用到 documentElement（首帧前调用可避免加载闪白）。 */
export function applyTheme(mode: ThemeMode): "light" | "dark" {
  const resolved = resolveTheme(mode);
  document.documentElement.dataset.theme = resolved;
  return resolved;
}
