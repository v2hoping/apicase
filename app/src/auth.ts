// 认证：显示元信息 + 需要「发送前/发送后」额外交互的两种方式（Digest、OAuth 2.0）。
//
// 分工：
//   - Basic / Bearer / API Key 是纯静态头，直接由 draft.ts 的 buildApiRequest 组装；
//   - Digest 必须先收到服务端 401 的 WWW-Authenticate challenge 才能算摘要；
//   - OAuth 2.0（客户端凭据）必须先去 token 端点换 access_token。
// 后两者都要发额外请求，因此统一收敛到 sendWithAuth——调用方只需给一个「怎么发」的回调，
// 这样 token 请求同样走后端 reqwest（绕开 CORS、复用代理设置）。
import { AuthType } from "./case";
import { ApiRequestPayload, HeaderEntry, ReqDraft, buildApiRequest, utf8Base64 } from "./draft";
import { md5 } from "./md5";

// ── 显示元信息（命名沿用 Postman / Insomnia / Bruno 的通行叫法）──
export interface AuthTypeMeta {
  value: AuthType;
  label: string; // 下拉与标题用英文通名
  zh: string; // 一句话中文说明
}

export const AUTH_TYPE_METAS: AuthTypeMeta[] = [
  { value: "none", label: "No Auth", zh: "不附加任何认证信息" },
  { value: "basic", label: "Basic Auth", zh: "用户名 + 密码，base64 编码后随请求头发送" },
  { value: "bearer", label: "Bearer Token", zh: "令牌（如 JWT）放入 Authorization 头" },
  { value: "apikey", label: "API Key", zh: "自定义键值，放在请求头或 URL 查询参数中" },
  { value: "digest", label: "Digest Auth", zh: "挑战-应答式摘要认证，密码不明文上网" },
  { value: "oauth2", label: "OAuth 2.0", zh: "客户端凭据模式：发送前自动换取 access_token" },
];

export function authLabel(t: AuthType): string {
  return AUTH_TYPE_METAS.find((m) => m.value === t)?.label ?? t;
}

/** 面板底部一行预览：这种认证方式最终往报文上附加什么，所见即所得，省得用户去猜。 */
export function authPreview(d: ReqDraft): { label: string; code: string } | null {
  switch (d.authType) {
    case "basic":
      return { label: "附加请求头", code: `Authorization: Basic ${utf8Base64(`${d.authBasicUser}:${d.authBasicPass}`)}` };
    case "bearer":
      return { label: "附加请求头", code: `Authorization: Bearer ${d.authBearerToken || "<token>"}` };
    case "apikey":
      return d.authApikeyIn === "header"
        ? { label: "附加请求头", code: `${d.authApikeyKey || "<键名>"}: ${d.authApikeyValue}` }
        : { label: "附加查询参数", code: `${d.authApikeyKey || "<键名>"}=${d.authApikeyValue}` };
    case "digest":
      return {
        label: "附加请求头",
        code: `Authorization: Digest username="${d.authDigestUser || "<用户名>"}", realm=…, response=…（收到 401 challenge 后计算并重发）`,
      };
    case "oauth2":
      return {
        label: "附加请求头",
        code: `Authorization: Bearer <access_token>（发送前向 ${d.authOauth2TokenUrl || "<token 端点>"} 换取）`,
      };
    default:
      return null;
  }
}

// ── Digest（RFC 2617 / 7616）────────────────────────
type Challenge = Record<string, string>;

/** 解析 `WWW-Authenticate: Digest realm="x", nonce="y", qop="auth"` 的参数表。 */
export function parseDigestChallenge(header: string): Challenge | null {
  const m = /(^|,)\s*Digest\s+/i.exec(header);
  if (!m) return null;
  const rest = header.slice(m.index + m[0].length);
  const out: Challenge = {};
  // key="quoted value" 或 key=token
  const re = /([A-Za-z][\w-]*)\s*=\s*(?:"((?:[^"\\]|\\.)*)"|([^,\s]+))/g;
  let p: RegExpExecArray | null;
  while ((p = re.exec(rest)) !== null) {
    out[p[1].toLowerCase()] = p[2] !== undefined ? p[2].replace(/\\(.)/g, "$1") : p[3];
  }
  return Object.keys(out).length ? out : null;
}

function randomCnonce(): string {
  const buf = new Uint8Array(8);
  crypto.getRandomValues(buf);
  let hex = "";
  for (const b of buf) hex += b.toString(16).padStart(2, "0");
  return hex;
}

/** URL 的 path + query（Digest 的 uri 参数与 HA2 都按它算）。 */
function requestUri(url: string): string {
  try {
    const u = new URL(url);
    return u.pathname + u.search;
  } catch {
    const i = url.indexOf("//");
    const j = url.indexOf("/", i >= 0 ? i + 2 : 0);
    return j >= 0 ? url.slice(j) : "/";
  }
}

/** 依据 challenge 计算 Authorization: Digest …（仅 qop=auth / 无 qop，auth-int 不支持）。 */
export function buildDigestHeader(
  ch: Challenge,
  opts: { username: string; password: string; method: string; url: string; cnonce?: string; nc?: string },
): string {
  const realm = ch.realm || "";
  const nonce = ch.nonce || "";
  const algorithm = (ch.algorithm || "MD5").toUpperCase();
  const qopList = (ch.qop || "").split(",").map((s) => s.trim().toLowerCase());
  const useQop = qopList.includes("auth");
  const uri = requestUri(opts.url);
  const cnonce = opts.cnonce ?? randomCnonce();
  const nc = opts.nc ?? "00000001";

  let ha1 = md5(`${opts.username}:${realm}:${opts.password}`);
  if (algorithm === "MD5-SESS") ha1 = md5(`${ha1}:${nonce}:${cnonce}`);
  const ha2 = md5(`${opts.method.toUpperCase()}:${uri}`);
  const response = useQop ? md5(`${ha1}:${nonce}:${nc}:${cnonce}:auth:${ha2}`) : md5(`${ha1}:${nonce}:${ha2}`);

  // 按 RFC：username/realm/nonce/uri/response/opaque 加引号，qop/nc/algorithm 不加
  const parts = [
    `username="${opts.username}"`,
    `realm="${realm}"`,
    `nonce="${nonce}"`,
    `uri="${uri}"`,
    `response="${response}"`,
  ];
  if (ch.algorithm) parts.push(`algorithm=${ch.algorithm}`);
  if (useQop) parts.push(`qop=auth`, `nc=${nc}`, `cnonce="${cnonce}"`);
  if (ch.opaque) parts.push(`opaque="${ch.opaque}"`);
  return `Digest ${parts.join(", ")}`;
}

// ── OAuth 2.0（client_credentials）──────────────────
interface CachedToken {
  token: string;
  type: string;
  expiresAt: number; // ms 时间戳；0 表示服务端未给 expires_in（不缓存过期，仅同次会话复用）
}
const tokenCache = new Map<string, CachedToken>();

function tokenCacheKey(d: ReqDraft): string {
  return [d.authOauth2TokenUrl, d.authOauth2ClientId, d.authOauth2Scope, d.authOauth2ClientAuth].join("|");
}

/** 清空 token 缓存（改动认证配置后调用，避免继续用旧令牌）。 */
export function clearTokenCache(): void {
  tokenCache.clear();
}

/** 最小响应形状：只要求这三样，避免与 App.tsx 的 ApiResponse 互相耦合。 */
export interface RespLike {
  status: number;
  headers: HeaderEntry[];
  body: string;
}
export type SendFn<R extends RespLike> = (payload: ApiRequestPayload) => Promise<R>;

async function fetchOAuth2Token<R extends RespLike>(d: ReqDraft, send: SendFn<R>): Promise<CachedToken> {
  const key = tokenCacheKey(d);
  const hit = tokenCache.get(key);
  if (hit && (hit.expiresAt === 0 || hit.expiresAt > Date.now())) return hit;

  if (!d.authOauth2TokenUrl.trim()) throw new Error("OAuth 2.0：请先填写 Access Token URL");
  const form = [`grant_type=client_credentials`];
  if (d.authOauth2Scope.trim()) form.push(`scope=${encodeURIComponent(d.authOauth2Scope.trim())}`);
  const headers: HeaderEntry[] = [{ key: "Content-Type", value: "application/x-www-form-urlencoded" }];
  if (d.authOauth2ClientAuth === "body") {
    form.push(`client_id=${encodeURIComponent(d.authOauth2ClientId)}`);
    form.push(`client_secret=${encodeURIComponent(d.authOauth2ClientSecret)}`);
  } else {
    headers.push({
      key: "Authorization",
      value: `Basic ${utf8Base64(`${d.authOauth2ClientId}:${d.authOauth2ClientSecret}`)}`,
    });
  }

  const resp = await send({ method: "POST", url: d.authOauth2TokenUrl.trim(), headers, body: form.join("&") });
  if (resp.status < 200 || resp.status >= 300) {
    throw new Error(`OAuth 2.0 取 token 失败（HTTP ${resp.status}）：${resp.body.slice(0, 200)}`);
  }
  let parsed: Record<string, unknown>;
  try {
    parsed = JSON.parse(resp.body) as Record<string, unknown>;
  } catch {
    throw new Error(`OAuth 2.0 取 token 失败：响应不是 JSON —— ${resp.body.slice(0, 200)}`);
  }
  const token = typeof parsed.access_token === "string" ? parsed.access_token : "";
  if (!token) throw new Error(`OAuth 2.0 取 token 失败：响应中没有 access_token —— ${resp.body.slice(0, 200)}`);
  const type = typeof parsed.token_type === "string" && parsed.token_type ? parsed.token_type : "Bearer";
  const ttl = typeof parsed.expires_in === "number" ? parsed.expires_in : Number(parsed.expires_in);
  // 提前 30s 过期，避免卡在边界上被服务端判过期
  const expiresAt = Number.isFinite(ttl) && ttl > 0 ? Date.now() + Math.max(0, ttl - 30) * 1000 : 0;
  const entry: CachedToken = { token, type: type.charAt(0).toUpperCase() + type.slice(1), expiresAt };
  tokenCache.set(key, entry);
  return entry;
}

// ── 统一发送入口 ────────────────────────────────────
/**
 * 按认证方式发送请求：静态头由 buildApiRequest 带上，Digest 与 OAuth 2.0 在此补齐。
 * send 由调用方注入（App 里就是后端 send_request），token 请求与重发因此同样走代理设置。
 */
export async function sendWithAuth<R extends RespLike>(d: ReqDraft, send: SendFn<R>): Promise<R> {
  const payload = buildApiRequest(d);

  if (d.authType === "oauth2") {
    const t = await fetchOAuth2Token(d, send);
    payload.headers.push({ key: "Authorization", value: `${t.type} ${t.token}` });
  }

  const resp = await send(payload);

  // Digest：首发必然吃 401，就着 challenge 算摘要重发一次（只重试一次，避免死循环）
  if (d.authType === "digest" && resp.status === 401) {
    const wwwAuth = resp.headers.find((h) => h.key.toLowerCase() === "www-authenticate")?.value || "";
    const ch = parseDigestChallenge(wwwAuth);
    if (ch) {
      const header = buildDigestHeader(ch, {
        username: d.authDigestUser,
        password: d.authDigestPass,
        method: payload.method,
        url: payload.url,
      });
      return await send({ ...payload, headers: [...payload.headers, { key: "Authorization", value: header }] });
    }
  }
  return resp;
}
