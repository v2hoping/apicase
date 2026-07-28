// 代理设置（app 级偏好）。后端 reqwest 据此决定发请求时是否走代理。
// mode：system=跟随系统代理（读 HTTP(S)_PROXY 环境变量）｜ none=不使用代理（直连）｜ custom=自定义地址。
// 持久化本身不在这里——统一由 settings.ts 写进应用配置目录的 settings.json（见该文件说明）。
export type ProxyMode = "system" | "none" | "custom";

export interface ProxyConfig {
  mode: ProxyMode;
  url: string; // custom 模式的代理地址，如 http://127.0.0.1:7890
}

export const DEFAULT_PROXY: ProxyConfig = { mode: "system", url: "" };

/** 任意来源（JSON / 旧 localStorage 值）→ 合法的代理配置；缺项与类型不符一律回默认。 */
export function normalizeProxyConfig(v: unknown): ProxyConfig {
  if (!v || typeof v !== "object") return { ...DEFAULT_PROXY };
  const o = v as Record<string, unknown>;
  const mode: ProxyMode = o.mode === "none" || o.mode === "custom" ? o.mode : "system";
  return { mode, url: typeof o.url === "string" ? o.url : "" };
}

// 传给后端 send_request 的 proxy 载荷：非 custom 时省略 url
export function proxyPayload(c: ProxyConfig): { mode: ProxyMode; url?: string } {
  return c.mode === "custom" ? { mode: c.mode, url: c.url.trim() } : { mode: c.mode };
}
