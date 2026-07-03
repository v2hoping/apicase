// 请求「编辑态草稿」（ReqDraft）：与 case.ts 的 HttpSpec 互转。
// 为什么要独立草稿：编辑 JSON body 时允许中途非法文本（不即时 parse），
// 故用字符串保存 body 文本，仅在保存/发送边界才校验并转回 HttpSpec。
// 单请求与多请求 flow 的每个请求都复用同一份 ReqDraft，请求编辑器因而完全通用。
import { HttpSpec, AuthSpec, AuthType, BodySpec, BodyType, KV, splitQueryFromUrl, mergeQueryIntoUrl } from "./case";

export interface ReqDraft {
  method: string;
  url: string; // 含 query（与 query 双向同步）
  query: KV[];
  headers: KV[];
  authType: AuthType;
  authBearerToken: string;
  authBasicUser: string;
  authBasicPass: string;
  authApikeyKey: string;
  authApikeyValue: string;
  authApikeyIn: "header" | "query";
  bodyType: BodyType;
  bodyText: string; // json / text 类型的编辑文本
  bodyContentType: string;
  bodyForm: KV[];
}

export function emptyDraft(method = "GET", url = ""): ReqDraft {
  return {
    method,
    url,
    query: splitQueryFromUrl(url).query,
    headers: [],
    authType: "none",
    authBearerToken: "",
    authBasicUser: "",
    authBasicPass: "",
    authApikeyKey: "",
    authApikeyValue: "",
    authApikeyIn: "header",
    bodyType: "none",
    bodyText: "",
    bodyContentType: "",
    bodyForm: [],
  };
}

/** HttpSpec → 编辑态草稿（打开 case / 切换请求时用）。 */
export function requestToDraft(r: HttpSpec): ReqDraft {
  const split = splitQueryFromUrl(r.url);
  const allQuery = [...split.query, ...r.query];
  return {
    method: r.method,
    url: mergeQueryIntoUrl(split.base, allQuery),
    query: allQuery,
    headers: r.headers,
    authType: r.auth.type,
    authBearerToken: r.auth.bearer?.token || "",
    authBasicUser: r.auth.basic?.username || "",
    authBasicPass: r.auth.basic?.password || "",
    authApikeyKey: r.auth.apikey?.key || "",
    authApikeyValue: r.auth.apikey?.value || "",
    authApikeyIn: r.auth.apikey?.in || "header",
    bodyType: r.body.type,
    bodyText:
      r.body.type === "json"
        ? r.body.json === undefined
          ? ""
          : JSON.stringify(r.body.json, null, 2)
        : r.body.type === "text"
          ? r.body.text || ""
          : "",
    bodyContentType: r.body.contentType || "",
    bodyForm: r.body.type === "form-urlencoded" ? r.body.urlencoded || [] : r.body.type === "form-data" ? r.body.formData || [] : [],
  };
}

function draftAuth(d: ReqDraft): AuthSpec {
  if (d.authType === "bearer") return { type: "bearer", bearer: { token: d.authBearerToken } };
  if (d.authType === "basic") return { type: "basic", basic: { username: d.authBasicUser, password: d.authBasicPass } };
  if (d.authType === "apikey")
    return { type: "apikey", apikey: { key: d.authApikeyKey, value: d.authApikeyValue, in: d.authApikeyIn } };
  return { type: "none" };
}

/** 草稿 → HttpSpec（保存边界，含 JSON body 校验）。 */
export function draftToRequest(d: ReqDraft): { request?: HttpSpec; error?: string } {
  let body: BodySpec;
  if (d.bodyType === "json") {
    if (d.bodyText.trim() === "") body = { type: "none" };
    else {
      try {
        body = { type: "json", json: JSON.parse(d.bodyText) };
      } catch {
        return { error: "Body JSON 格式非法，无法保存" };
      }
    }
  } else if (d.bodyType === "text") {
    body = { type: "text", text: d.bodyText, contentType: d.bodyContentType || undefined };
  } else if (d.bodyType === "form-urlencoded") {
    body = { type: "form-urlencoded", urlencoded: d.bodyForm };
  } else if (d.bodyType === "form-data") {
    body = { type: "form-data", formData: d.bodyForm };
  } else {
    body = { type: "none" };
  }
  const request: HttpSpec = {
    method: d.method,
    url: splitQueryFromUrl(d.url.trim()).base,
    query: d.query,
    headers: d.headers,
    auth: draftAuth(d),
    body,
  };
  return { request };
}

// ── 组装实际发送的请求（合并 auth、body → 后端 send_request 载荷）──
export interface HeaderEntry {
  key: string;
  value: string;
}
export interface ApiRequestPayload {
  method: string;
  url: string;
  headers: HeaderEntry[];
  body: string | null;
}

export function buildApiRequest(d: ReqDraft): ApiRequestPayload {
  let finalUrl = d.url.trim();
  const hdrs: HeaderEntry[] = d.headers
    .filter((h) => h.enabled !== false && h.name.trim() !== "")
    .map((h) => ({ key: h.name.trim(), value: h.value }));
  const hasContentType = () => hdrs.some((h) => h.key.toLowerCase() === "content-type");
  // auth
  if (d.authType === "bearer" && d.authBearerToken) {
    hdrs.push({ key: "Authorization", value: `Bearer ${d.authBearerToken}` });
  } else if (d.authType === "basic") {
    hdrs.push({ key: "Authorization", value: `Basic ${btoa(`${d.authBasicUser}:${d.authBasicPass}`)}` });
  } else if (d.authType === "apikey" && d.authApikeyKey) {
    if (d.authApikeyIn === "header") {
      hdrs.push({ key: d.authApikeyKey, value: d.authApikeyValue });
    } else {
      const cur = splitQueryFromUrl(finalUrl);
      finalUrl = mergeQueryIntoUrl(finalUrl, [...cur.query, { name: d.authApikeyKey, value: d.authApikeyValue, enabled: true }]);
    }
  }
  // body
  let bodyStr: string | null = null;
  if (d.bodyType === "json" && d.bodyText.trim() !== "") {
    bodyStr = d.bodyText;
    if (!hasContentType()) hdrs.push({ key: "Content-Type", value: "application/json" });
  } else if (d.bodyType === "text" && d.bodyText !== "") {
    bodyStr = d.bodyText;
    if (d.bodyContentType && !hasContentType()) hdrs.push({ key: "Content-Type", value: d.bodyContentType });
  } else if (d.bodyType === "form-urlencoded") {
    const pairs = d.bodyForm.filter((k) => k.enabled !== false && k.name.trim() !== "");
    if (pairs.length) {
      bodyStr = pairs.map((k) => `${encodeURIComponent(k.name)}=${encodeURIComponent(k.value)}`).join("&");
      if (!hasContentType()) hdrs.push({ key: "Content-Type", value: "application/x-www-form-urlencoded" });
    }
  }
  // form-data 的 multipart 发送暂未实现（保存可用），此处不组装 body
  return { method: d.method, url: finalUrl, headers: hdrs, body: bodyStr };
}
