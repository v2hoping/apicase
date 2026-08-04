// 应用级设置（settings.json）的读取与迁移单测。
// 这里最怕的是「用户设置悄悄丢了」：旧版本各偏好各存一个 localStorage 键，
// 迁移到 settings.json 时若回落逻辑写错，用户的主题 / 代理 / 快捷键就凭空归默认。
// 故重点覆盖：旧键回落、缓存优先级、逐字段独立兜底。
//
// 环境说明：Node 里没有 localStorage，用内存实现顶上；@tauri-apps 的 invoke 在非 Tauri 环境会抛错，
// 正好走 loadAppSettings 的 catch 分支——即「读不到 settings.json 时应回落到旧键」这条路径。
import { loadModule, eq, ok, report } from "./harness.mjs";

class MemStorage {
  #m = new Map();
  getItem(k) {
    return this.#m.has(k) ? this.#m.get(k) : null;
  }
  setItem(k, v) {
    this.#m.set(k, String(v));
  }
  removeItem(k) {
    this.#m.delete(k);
  }
  clear() {
    this.#m.clear();
  }
  has(k) {
    return this.#m.has(k);
  }
}
const store = new MemStorage();
globalThis.localStorage = store;

const { loadCachedSettings, loadAppSettings, DEFAULT_APP_SETTINGS } = await loadModule("src/settings.ts");

const CACHE = "apicase.settings.cache.v1";
const LEGACY = {
  theme: "apicase.theme.v1",
  proxy: "apicase.proxy.v1",
  shortcuts: "apicase.shortcuts.v1",
  enabled: "apicase.shortcuts.enabled.v1",
};
const DEFAULTS = {
  recentWorkspaces: [],
  theme: "system",
  proxy: { mode: "system", url: "" },
  shortcuts: {},
  shortcutsEnabled: true,
  showHiddenFiles: false,
  // 默认关：AGENTS.md 会随 git 走，往用户的仓库里自动塞文件是冒犯
  aiAutoSetup: false,
};

eq(DEFAULT_APP_SETTINGS, DEFAULTS, "默认设置");

// ── 空环境 ──
store.clear();
eq(loadCachedSettings(), DEFAULTS, "无任何存储时回默认");

// ── 旧键迁移：这是重构的核心承诺，老用户升级后设置不能丢 ──
store.clear();
store.setItem(LEGACY.theme, "dark");
store.setItem(LEGACY.proxy, JSON.stringify({ mode: "custom", url: "http://127.0.0.1:7890" }));
store.setItem(LEGACY.shortcuts, JSON.stringify({ save: "Mod+S", run: "" }));
store.setItem(LEGACY.enabled, "0");
const migrated = loadCachedSettings();
eq(migrated.theme, "dark", "旧键的主题被迁移");
eq(migrated.proxy, { mode: "custom", url: "http://127.0.0.1:7890" }, "旧键的代理被迁移");
eq(migrated.shortcuts, { save: "Mod+S", run: "" }, "旧键的快捷键绑定被迁移（含显式禁用的空串）");
eq(migrated.shortcutsEnabled, false, '旧格式 "0" 迁移为关闭');

// 旧格式 "1" 与缺省都算启用
store.setItem(LEGACY.enabled, "1");
eq(loadCachedSettings().shortcutsEnabled, true, '旧格式 "1" 迁移为启用');
store.removeItem(LEGACY.enabled);
eq(loadCachedSettings().shortcutsEnabled, true, "旧键缺失时默认启用");

// 非 Tauri 环境下 invoke 抛错 → loadAppSettings 同样要回落到旧键，而不是一把归默认
const viaDisk = await loadAppSettings();
eq(viaDisk.theme, "dark", "读不到 settings.json 时仍从旧键回落");
eq(viaDisk.proxy.mode, "custom", "读不到 settings.json 时代理也回落");

// ── 缓存优先于旧键（缓存是 settings.json 的镜像，比旧键新）──
store.clear();
store.setItem(LEGACY.theme, "dark");
store.setItem(CACHE, JSON.stringify({ ...DEFAULTS, theme: "light" }));
eq(loadCachedSettings().theme, "light", "缓存存在时以缓存为准");

// 缓存里缺的字段仍从旧键补——迁移过程中可能只写了一半
store.clear();
store.setItem(LEGACY.proxy, JSON.stringify({ mode: "none", url: "" }));
store.setItem(CACHE, JSON.stringify({ theme: "dark" }));
const partial = loadCachedSettings();
eq(partial.theme, "dark", "缓存里有的字段用缓存");
eq(partial.proxy.mode, "none", "缓存里没有的字段回落旧键");
eq(partial.shortcuts, {}, "两边都没有的字段用默认");

// ── 逐字段独立兜底：一处写坏不该带塌其余 ──
store.clear();
store.setItem(
  CACHE,
  JSON.stringify({
    recentWorkspaces: ["/a", 42, "/b"], // 混入非字符串
    theme: "紫色", // 非法枚举
    proxy: "不是对象",
    shortcuts: { save: "Mod+S", bad: 123 }, // 混入非字符串值
    shortcutsEnabled: "yes", // 非布尔
  }),
);
const messy = loadCachedSettings();
eq(messy.recentWorkspaces, ["/a", "/b"], "最近工作空间剔除非字符串项");
eq(messy.theme, "system", "非法主题回默认");
eq(messy.proxy, { mode: "system", url: "" }, "代理非对象回默认");
eq(messy.shortcuts, { save: "Mod+S" }, "快捷键表丢弃非字符串值、保留合法项");
eq(messy.shortcutsEnabled, true, "非布尔的开关按启用兜底（仅显式 false 才关闭）");

// 显式 false 才关闭
store.setItem(CACHE, JSON.stringify({ shortcutsEnabled: false }));
eq(loadCachedSettings().shortcutsEnabled, false, "显式 false 才关闭快捷键");

// ── 显示隐藏文件：仅显式 true 才开（缺失 / 写错类型都按关闭这一保守侧兜底）──
store.clear();
eq(loadCachedSettings().showHiddenFiles, false, "缺字段时默认不显示隐藏文件");
store.setItem(CACHE, JSON.stringify({ showHiddenFiles: true }));
eq(loadCachedSettings().showHiddenFiles, true, "显式 true 才显示隐藏文件");
// aiAutoSetup 只有显式 true 才开（同 showHiddenFiles）——它会往仓库里写文件，不能靠猜
store.setItem(CACHE, JSON.stringify({ aiAutoSetup: true }));
eq(loadCachedSettings().aiAutoSetup, true, "显式 true 才自动补齐");
store.setItem(CACHE, JSON.stringify({ aiAutoSetup: "yes" }));
eq(loadCachedSettings().aiAutoSetup, false, "非布尔值不算开");
store.setItem(CACHE, JSON.stringify({ showHiddenFiles: "yes" }));
eq(loadCachedSettings().showHiddenFiles, false, "类型写错按关闭处理");

// ── 坏 JSON / 非对象不抛错 ──
store.clear();
store.setItem(CACHE, "{ 这不是 JSON");
eq(loadCachedSettings(), DEFAULTS, "缓存是坏 JSON 时回默认，不抛错");
store.setItem(CACHE, JSON.stringify([1, 2, 3]));
eq(loadCachedSettings(), DEFAULTS, "缓存是数组（非对象）时回默认");
store.setItem(LEGACY.proxy, "{ 坏的");
ok(loadCachedSettings().proxy.mode === "system", "旧键是坏 JSON 时回默认，不抛错");

report();
