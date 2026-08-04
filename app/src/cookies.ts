// Cookie jar 的类型与 IPC 封装（设置页「Cookies」用）。
//
// jar 本身在 Rust（`core/src/cookie.rs`）：收发、域/路径匹配、合法性校验、持久化都在那边，
// 前端只负责把 jar 的路径递下去、把回来的列表画出来。
import { invoke } from "@tauri-apps/api/core";
import { formatDateTime } from "./datetime";

/** jar 相对工作空间根的位置（与报告目录同在 `.apicase/` 下，已随它一起进 .gitignore）。 */
export const COOKIE_JAR_REL = ".apicase/cookies.json";

/**
 * 一条 cookie。`domain + path + name` 是主键——只按 name 删会误伤同名不同域的那条。
 */
export interface CookieItem {
  domain: string;
  path: string;
  name: string;
  value: string;
  secure: boolean;
  httpOnly: boolean;
  /** 过期时间（Unix 毫秒）；缺省 = 会话 cookie */
  expiresMs?: number;
  /** 已过期：不会再被发送，但仍列出来——否则用户看到「没有 cookie」却又删不掉它 */
  expired: boolean;
  /** 无 Domain 属性 = 只发给这一个主机（有则子域一并生效） */
  hostOnly: boolean;
}

/** cookie 的主键（改名 / 换域时用来删掉原来那条）。 */
export interface CookieKey {
  domain: string;
  path: string;
  name: string;
}

/** 提交给后端的一条 cookie：`domain` 以 `.` 开头表示子域一并生效。 */
export interface CookieInput {
  domain: string;
  path: string;
  name: string;
  value: string;
  secure: boolean;
  httpOnly: boolean;
  expiresMs?: number;
}

/** 读回全部 cookie（按 域 → 路径 → 名 排序，含会话与已过期的）。 */
export function listCookies(jarPath: string): Promise<CookieItem[]> {
  return invoke<CookieItem[]>("list_cookies", { jarPath });
}

/**
 * 新增或修改。`prev` 是修改前的主键（新增时省略）。
 * 校验在 Rust（与真实响应走同一套解析），失败时抛出可直接展示的中文错误。
 */
export function saveCookie(jarPath: string, cookie: CookieInput, prev?: CookieKey): Promise<void> {
  return invoke("save_cookie", { jarPath, prev: prev ?? null, cookie });
}

/** 删一条，返回是否真的删掉了。 */
export function deleteCookie(jarPath: string, c: CookieKey): Promise<boolean> {
  return invoke<boolean>("delete_cookie", { jarPath, domain: c.domain, path: c.path, name: c.name });
}

/** 清空：`domain` 给了就只清该域，否则全清。返回清掉的条数。 */
export function clearCookies(jarPath: string, domain?: string): Promise<number> {
  return invoke<number>("clear_cookies", { jarPath, domain: domain ?? null });
}

/** 按域分组（列表已排好序，故分组内也保持 路径 → 名 的顺序）。 */
export function groupByDomain(items: CookieItem[]): { domain: string; items: CookieItem[] }[] {
  const out: { domain: string; items: CookieItem[] }[] = [];
  for (const it of items) {
    const last = out[out.length - 1];
    if (last && last.domain === it.domain) last.items.push(it);
    else out.push({ domain: it.domain, items: [it] });
  }
  return out;
}

/**
 * 搜索：域 / 名 / 值 任一命中即保留（不区分大小写）。
 * 三个字段一起搜而不是只搜域名——「哪个 cookie 里存着这个 token」和
 * 「这个站点有哪些 cookie」是同样常见的两个问题。
 */
export function filterCookies(items: CookieItem[], query: string): CookieItem[] {
  const q = query.trim().toLowerCase();
  if (!q) return items;
  return items.filter((c) =>
    [c.domain, c.name, c.value].some((f) => f.toLowerCase().includes(q)),
  );
}

/** 编辑框里显示的域：带 Domain 属性的写成 `.example.com`，以免编辑一次就把子域通配丢了。 */
export function domainForEdit(c: CookieItem): string {
  return c.hostOnly ? c.domain : `.${c.domain}`;
}

/**
 * 过期时间的显示文本：会话 cookie 写「会话」。
 * 格式与编辑控件里显示的是同一份（`formatDateTime`）——列表与编辑框对不上号最容易让人以为改错了。
 */
export function expiryText(c: CookieItem): string {
  return c.expiresMs ? formatDateTime(c.expiresMs) : "会话";
}
