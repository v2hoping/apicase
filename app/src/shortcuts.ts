// 全局快捷键：动作注册表 + 键位解析 / 格式化 / 持久化 / 冲突检测（纯逻辑，无 React）。
// 设计：一套跨平台配置——修饰键统一抽象为 Mod（macOS=⌘，其它=Ctrl）。
// 用户自定义 override 存 localStorage；未 override 用默认；override 为空串表示禁用。

export type ActionId =
  | "new-case"
  | "open-workspace"
  | "save"
  | "close-tab"
  | "search"
  | "open-settings"
  | "send-request";

/** 规范化的快捷键组合。key 为小写主键（字母 / 数字 / 符号原样，特殊键如 "enter"）。 */
export interface Accel {
  mod: boolean; // ⌘(mac) / Ctrl(win/linux)
  alt: boolean;
  shift: boolean;
  key: string;
}

export interface ActionDef {
  id: ActionId;
  label: string;
  group: string;
  def: Accel;
}

export const IS_MAC =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || "");

function A(key: string, opts: Partial<Omit<Accel, "key">> = {}): Accel {
  return { mod: !!opts.mod, alt: !!opts.alt, shift: !!opts.shift, key };
}

/** 动作注册表（核心动作）。顺序即配置页展示顺序。 */
export const ACTIONS: ActionDef[] = [
  { id: "new-case", label: "新建用例", group: "文件", def: A("n", { mod: true }) },
  { id: "open-workspace", label: "打开工作空间", group: "文件", def: A("o", { mod: true }) },
  { id: "save", label: "保存", group: "文件", def: A("s", { mod: true }) },
  { id: "close-tab", label: "关闭当前标签", group: "文件", def: A("w", { mod: true }) },
  { id: "search", label: "搜索用例", group: "导航", def: A("p", { mod: true }) },
  { id: "open-settings", label: "打开配置页", group: "导航", def: A(",", { mod: true }) },
  { id: "send-request", label: "发送请求", group: "运行", def: A("enter", { mod: true }) },
];

export const ACTION_MAP: Record<ActionId, ActionDef> = Object.fromEntries(
  ACTIONS.map((a) => [a.id, a]),
) as Record<ActionId, ActionDef>;

const MODIFIER_KEYS = new Set(["Shift", "Control", "Alt", "Meta"]);

/** 从键盘事件提取规范化组合；只按修饰键时返回 null。 */
export function eventToAccel(e: KeyboardEvent): Accel | null {
  if (MODIFIER_KEYS.has(e.key)) return null;
  let key = e.key.toLowerCase(); // 字母/符号原样小写；"Enter"→"enter"、"ArrowUp"→"arrowup"
  if (key === " ") key = "space";
  return { mod: e.metaKey || e.ctrlKey, alt: e.altKey, shift: e.shiftKey, key };
}

/** 规范化字符串（用于比较 / 存储 / 查表）。无主键 → ""。 */
export function accelKey(a: Accel | null): string {
  if (!a || !a.key) return "";
  const parts: string[] = [];
  if (a.mod) parts.push("mod");
  if (a.alt) parts.push("alt");
  if (a.shift) parts.push("shift");
  parts.push(a.key);
  return parts.join("+");
}

/** 从规范化字符串还原 Accel；空串 → null。 */
export function parseAccel(s: string): Accel | null {
  if (!s) return null;
  const parts = s.split("+");
  const key = parts.pop() || "";
  if (!key) return null;
  return { mod: parts.includes("mod"), alt: parts.includes("alt"), shift: parts.includes("shift"), key };
}

const KEY_LABEL: Record<string, string> = {
  enter: "Enter",
  escape: "Esc",
  space: "Space",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
  backspace: "⌫",
  delete: "Del",
  tab: "Tab",
};

function keyLabel(key: string): string {
  if (KEY_LABEL[key]) return KEY_LABEL[key];
  if (key.length === 1) return key.toUpperCase();
  return key.charAt(0).toUpperCase() + key.slice(1);
}

/** 人类可读展示：mac 用符号（⌘⇧⌥），其它用 Ctrl+…。null / 未绑定 → "未设置"。 */
export function formatAccel(a: Accel | null): string {
  if (!a || !a.key) return "未设置";
  if (IS_MAC) {
    let s = "";
    if (a.mod) s += "⌘";
    if (a.alt) s += "⌥";
    if (a.shift) s += "⇧";
    return s + keyLabel(a.key);
  }
  const parts: string[] = [];
  if (a.mod) parts.push("Ctrl");
  if (a.alt) parts.push("Alt");
  if (a.shift) parts.push("Shift");
  parts.push(keyLabel(a.key));
  return parts.join("+");
}

/** 拆成单个按键 token（每个键单独渲染一个框）。null / 未绑定 → []。 */
export function accelTokens(a: Accel | null): string[] {
  if (!a || !a.key) return [];
  const out: string[] = [];
  if (IS_MAC) {
    if (a.mod) out.push("⌘");
    if (a.alt) out.push("⌥");
    if (a.shift) out.push("⇧");
  } else {
    if (a.mod) out.push("Ctrl");
    if (a.alt) out.push("Alt");
    if (a.shift) out.push("Shift");
  }
  out.push(keyLabel(a.key));
  return out;
}

// ── 持久化（localStorage）─────────────────────────
// override 值：accel 字符串（自定义）| ""（显式禁用）；缺省 key → 用默认。
const LS_KEY = "apicase.shortcuts.v1";
// 快捷键功能总开关（关闭时全局不分发任何快捷键）；缺省视为启用。
const LS_ENABLED_KEY = "apicase.shortcuts.enabled.v1";

export type Overrides = Partial<Record<ActionId, string>>;

export function loadShortcutsEnabled(): boolean {
  return localStorage.getItem(LS_ENABLED_KEY) !== "0";
}

export function saveShortcutsEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(LS_ENABLED_KEY, enabled ? "1" : "0");
  } catch {
    /* ignore */
  }
}

export function loadOverrides(): Overrides {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return {};
    const o = JSON.parse(raw);
    if (o && typeof o === "object") return o as Overrides;
  } catch {
    /* ignore */
  }
  return {};
}

export function saveOverrides(o: Overrides): void {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify(o));
  } catch {
    /* ignore */
  }
}

/** 合并默认 + override → 每个动作的生效键位（null = 禁用）。 */
export function resolveBindings(o: Overrides): Record<ActionId, Accel | null> {
  const out = {} as Record<ActionId, Accel | null>;
  for (const a of ACTIONS) {
    if (Object.prototype.hasOwnProperty.call(o, a.id)) {
      const v = o[a.id]!;
      out[a.id] = v === "" ? null : parseAccel(v);
    } else {
      out[a.id] = a.def;
    }
  }
  return out;
}

/** 反查表：规范化字符串 → 动作 id（用于全局分发）。 */
export function buildLookup(b: Record<ActionId, Accel | null>): Record<string, ActionId> {
  const m: Record<string, ActionId> = {};
  for (const a of ACTIONS) {
    const k = accelKey(b[a.id]);
    if (k) m[k] = a.id;
  }
  return m;
}

/** 冲突检测：返回占用该键位的其它动作 id（无冲突 → null）。 */
export function findConflict(b: Record<ActionId, Accel | null>, accel: Accel, exceptId: ActionId): ActionId | null {
  const k = accelKey(accel);
  if (!k) return null;
  for (const a of ACTIONS) {
    if (a.id === exceptId) continue;
    if (accelKey(b[a.id]) === k) return a.id;
  }
  return null;
}

/** 该动作当前是否为默认键位（用于「恢复默认」按钮显隐）。 */
export function isDefaultBinding(o: Overrides, id: ActionId): boolean {
  return !Object.prototype.hasOwnProperty.call(o, id);
}
