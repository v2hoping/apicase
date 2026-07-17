// 主题（浅色 / 深色 / 跟随系统）：模式持久化到 localStorage，
// 解析出的实际明暗写到 documentElement 的 data-theme，供 CSS 的 :root[data-theme=dark] 覆盖变量。
export type ThemeMode = "light" | "dark" | "system";

const LS_KEY = "apicase.theme.v1";

export function loadThemeMode(): ThemeMode {
  const v = localStorage.getItem(LS_KEY);
  return v === "light" || v === "dark" || v === "system" ? v : "system";
}

export function saveThemeMode(mode: ThemeMode): void {
  try {
    localStorage.setItem(LS_KEY, mode);
  } catch {
    /* ignore */
  }
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
