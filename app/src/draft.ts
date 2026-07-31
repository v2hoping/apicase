// 请求「编辑态草稿」（ReqDraft）：与 case.ts 的 HttpSpec 互转。
// 为什么要独立草稿：编辑 JSON body 时允许中途非法文本（不即时 parse），
// 故用字符串保存 body 文本，仅在保存/发送边界才校验并转回 HttpSpec。
// 单请求与多请求 flow 的每个请求都复用同一份 ReqDraft，请求编辑器因而完全通用。
//
// **这里只负责编辑态 ↔ HttpSpec 的互转**（纯前端、同步，服务于表单交互）。
// 「HttpSpec → 真正发出去的报文」那一步在 Rust（`core/src/request.rs`）——
// 认证头怎么加、Content-Type 怎么定、multipart 怎么组，都不在前端。
import {
  HttpSpec,
  AuthSpec,
  AuthType,
  BodySpec,
  BodyType,
  KV,
  FormItem,
  Case,
  Request,
  RequestOutput,
  Assertion,
  UiNodes,
  splitQueryFromUrl,
  mergeQueryIntoUrl,
} from "./case";
import { baseName } from "./pathutil";

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
  authDigestUser: string;
  authDigestPass: string;
  authOauth2TokenUrl: string;
  authOauth2ClientId: string;
  authOauth2ClientSecret: string;
  authOauth2Scope: string;
  authOauth2ClientAuth: "header" | "body";
  bodyType: BodyType;
  bodyText: string; // json / xml / text 类型的编辑文本
  bodyContentType: string; // text / binary 可选覆盖 Content-Type
  bodyForm: FormItem[]; // form-urlencoded / form-data 共用；仅后者认 type: file
  bodyFilePath: string; // binary：以原始字节发送的文件路径
}

/** 各请求体类型默认带的 Content-Type；form-data 由后端 reqwest 生成（含 boundary），故为空。
 *  文本类显式带 charset=utf-8（对齐新版 Postman）：body 本就以 UTF-8 字节发送，
 *  声明 charset 可规避 xml/text 「无 charset 默认非 UTF-8」的历史坑，接收端稳解中文。 */
export const DEFAULT_CONTENT_TYPE: Partial<Record<BodyType, string>> = {
  json: "application/json; charset=utf-8",
  xml: "application/xml; charset=utf-8",
  text: "text/plain; charset=utf-8",
  "form-urlencoded": "application/x-www-form-urlencoded",
};

// 文件扩展名 → MIME（对齐 Postman：binary 选文件后按类型自动定 Content-Type）
const EXT_CONTENT_TYPE: Record<string, string> = {
  json: "application/json",
  xml: "application/xml",
  txt: "text/plain",
  csv: "text/csv",
  html: "text/html",
  htm: "text/html",
  css: "text/css",
  js: "text/javascript",
  md: "text/markdown",
  pdf: "application/pdf",
  zip: "application/zip",
  gz: "application/gzip",
  tar: "application/x-tar",
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  bmp: "image/bmp",
  ico: "image/x-icon",
  mp3: "audio/mpeg",
  wav: "audio/wav",
  mp4: "video/mp4",
  mov: "video/quicktime",
  webm: "video/webm",
  doc: "application/msword",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  xls: "application/vnd.ms-excel",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  ppt: "application/vnd.ms-powerpoint",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
};

/** binary 请求体：按文件扩展名推断 Content-Type，推不出兜底 application/octet-stream（同 Postman）。 */
export function guessContentType(path: string): string {
  const m = /\.([A-Za-z0-9]+)\s*$/.exec(path.trim());
  const ext = m ? m[1].toLowerCase() : "";
  return EXT_CONTENT_TYPE[ext] || "application/octet-stream";
}

// multipart 文件 part 的 filename 取路径最后一段，与文件树共用同一份实现（见 pathutil.ts）
export { baseName };

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
    authDigestUser: "",
    authDigestPass: "",
    authOauth2TokenUrl: "",
    authOauth2ClientId: "",
    authOauth2ClientSecret: "",
    authOauth2Scope: "",
    authOauth2ClientAuth: "header",
    bodyType: "none",
    bodyText: "",
    bodyContentType: "",
    bodyForm: [],
    bodyFilePath: "",
  };
}

/**
 * case 内部统一模型：一个 step 的编辑态（单请求 = 只有 1 个）。
 * 请求报文部分复用 ReqDraft，故请求编辑器对单节点与 flow step 完全通用。
 */
export interface RequestDraft {
  id: string;
  protocol: string;
  dependsOn: string[];
  outputs: RequestOutput[];
  assertions: Assertion[];
  docs: string;
  req: ReqDraft;
}

/** valid 但 http 报文缺失时的兜底（极少数） */
function emptyHttpSpec(): HttpSpec {
  return { method: "GET", url: "", query: [], headers: [], auth: { type: "none" }, body: { type: "none" } };
}

/** Case → step 编辑态列表（+ 画布坐标）。空 steps 兜一个空请求，保证编辑器恒有内容可渲染。 */
export function caseToRequests(c: Case): { requests: RequestDraft[]; ui?: UiNodes } {
  const src: Request[] = c.requests.length
    ? c.requests
    : [{ id: "step1", protocol: "http", http: emptyHttpSpec(), dependsOn: [], outputs: [], assertions: [] }];
  return {
    requests: src.map((r) => ({
      id: r.id,
      protocol: r.protocol || "http",
      dependsOn: r.dependsOn,
      outputs: r.outputs,
      assertions: r.assertions,
      docs: r.docs || "",
      req: requestToDraft(r.http),
    })),
    ui: c.ui?.nodes,
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
    authDigestUser: r.auth.digest?.username || "",
    authDigestPass: r.auth.digest?.password || "",
    authOauth2TokenUrl: r.auth.oauth2?.tokenUrl || "",
    authOauth2ClientId: r.auth.oauth2?.clientId || "",
    authOauth2ClientSecret: r.auth.oauth2?.clientSecret || "",
    authOauth2Scope: r.auth.oauth2?.scope || "",
    authOauth2ClientAuth: r.auth.oauth2?.clientAuth || "header",
    bodyType: r.body.type,
    bodyText:
      r.body.type === "json"
        ? r.body.json === undefined
          ? ""
          : JSON.stringify(r.body.json, null, 2)
        : r.body.type === "xml"
          ? r.body.xml || ""
          : r.body.type === "text"
            ? r.body.text || ""
            : "",
    bodyContentType: r.body.contentType || "",
    bodyForm: r.body.type === "form-urlencoded" ? r.body.urlencoded || [] : r.body.type === "form-data" ? r.body.formData || [] : [],
    bodyFilePath: r.body.filePath || "",
  };
}

function draftAuth(d: ReqDraft): AuthSpec {
  if (d.authType === "bearer") return { type: "bearer", bearer: { token: d.authBearerToken } };
  if (d.authType === "basic") return { type: "basic", basic: { username: d.authBasicUser, password: d.authBasicPass } };
  if (d.authType === "apikey")
    return { type: "apikey", apikey: { key: d.authApikeyKey, value: d.authApikeyValue, in: d.authApikeyIn } };
  if (d.authType === "digest")
    return { type: "digest", digest: { username: d.authDigestUser, password: d.authDigestPass } };
  if (d.authType === "oauth2")
    return {
      type: "oauth2",
      oauth2: {
        tokenUrl: d.authOauth2TokenUrl,
        clientId: d.authOauth2ClientId,
        clientSecret: d.authOauth2ClientSecret,
        scope: d.authOauth2Scope || undefined,
        clientAuth: d.authOauth2ClientAuth,
      },
    };
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
  } else if (d.bodyType === "xml") {
    body = { type: "xml", xml: d.bodyText };
  } else if (d.bodyType === "text") {
    body = { type: "text", text: d.bodyText, contentType: d.bodyContentType || undefined };
  } else if (d.bodyType === "binary") {
    body = { type: "binary", filePath: d.bodyFilePath, contentType: d.bodyContentType || undefined };
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

/**
 * UTF-8 安全的 base64。`btoa` 只吃 Latin-1，含中文的用户名 / 密码会直接抛错。
 * 前端只在**认证面板的预览**里用它（"这种认证最终往报文上附加什么"）；
 * 真正发送时的编码在 Rust（`core/src/request.rs`），两处互不依赖。
 */
export function utf8Base64(s: string): string {
  let bin = "";
  for (const b of new TextEncoder().encode(s)) bin += String.fromCharCode(b);
  return btoa(bin);
}
