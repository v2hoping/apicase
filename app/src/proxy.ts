// 代理设置（app 级偏好，持久化 localStorage）。后端 reqwest 据此决定发请求时是否走代理。
// mode：system=跟随系统代理（读 HTTP(S)_PROXY 环境变量）｜ none=不使用代理（直连）｜ custom=自定义地址。
export type ProxyMode = "system" | "none" | "custom";

export interface ProxyConfig {
  mode: ProxyMode;
  url: string; // custom 模式的代理地址，如 http://127.0.0.1:7890
}

const KEY = "apicase.proxy.v1";

export const DEFAULT_PROXY: ProxyConfig = { mode: "system", url: "" };

export function loadProxyConfig(): ProxyConfig {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULT_PROXY };
    const o = JSON.parse(raw);
    const mode: ProxyMode = o.mode === "none" || o.mode === "custom" ? o.mode : "system";
    return { mode, url: typeof o.url === "string" ? o.url : "" };
  } catch {
    return { ...DEFAULT_PROXY };
  }
}

export function saveProxyConfig(c: ProxyConfig): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(c));
  } catch {
    /* ignore */
  }
}

// 传给后端 send_request 的 proxy 载荷：非 custom 时省略 url
export function proxyPayload(c: ProxyConfig): { mode: ProxyMode; url?: string } {
  return c.mode === "custom" ? { mode: c.mode, url: c.url.trim() } : { mode: c.mode };
}
