// auth.ts 单元测试：MD5 向量、Digest challenge 解析与摘要计算（RFC 2617 官方示例）、
// OAuth 2.0 客户端凭据流程（含 token 缓存）、以及两者在 sendWithAuth 中的编排。
import { createHash } from "node:crypto";
import { loadModule, eq, ok, has, hasnt, report } from "./harness.mjs";

const { md5 } = await loadModule("src/md5.ts");
const { parseDigestChallenge, buildDigestHeader, sendWithAuth, clearTokenCache, authPreview, AUTH_TYPE_METAS } =
  await loadModule("src/auth.ts");
const { emptyDraft } = await loadModule("src/draft.ts");
const { parseCase, dumpCase } = await loadModule("src/case.ts");

// ── 1. MD5 ──
for (const s of ["", "abc", "A".repeat(55), "A".repeat(56), "A".repeat(64), "中文密码:realm:pass"]) {
  eq(md5(s), createHash("md5").update(s, "utf8").digest("hex"), `md5(${JSON.stringify(s.slice(0, 12))})`);
}

// ── 2. Digest challenge 解析 ──
const ch = parseDigestChallenge(
  'Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41"',
);
eq(ch.realm, "testrealm@host.com", "解析 realm");
eq(ch.nonce, "dcd98b7102dd2f0e8b11d0f600bfb0c093", "解析 nonce");
eq(ch.qop, "auth,auth-int", "解析 qop");
eq(ch.opaque, "5ccc069c403ebaf9f0171e9517f40e41", "解析 opaque");
eq(parseDigestChallenge('Basic realm="x"'), null, "非 Digest challenge 返回 null");
eq(parseDigestChallenge("Digest realm=simple, algorithm=MD5").algorithm, "MD5", "解析不带引号的参数值");

// ── 3. Digest 摘要：RFC 2617 §3.5 官方示例 ──
const hdr = buildDigestHeader(ch, {
  username: "Mufasa",
  password: "Circle Of Life",
  method: "GET",
  url: "http://www.nowhere.org/dir/index.html",
  cnonce: "0a4f113b",
  nc: "00000001",
});
has(hdr, 'response="6629fae49393a05397450978507c4ef1"', "RFC 2617 示例的 response 一致");
has(hdr, 'uri="/dir/index.html"', "uri 取 path（不含 host）");
has(hdr, "qop=auth", "qop 不加引号");
has(hdr, 'opaque="5ccc069c403ebaf9f0171e9517f40e41"', "回带 opaque");
// 无 qop 的老式服务端：response = MD5(HA1:nonce:HA2)
const noQop = buildDigestHeader({ realm: "r", nonce: "n" }, { username: "u", password: "p", method: "GET", url: "http://h/x?a=1" });
const ha1 = md5("u:r:p");
const ha2 = md5("GET:/x?a=1");
has(noQop, `response="${md5(`${ha1}:n:${ha2}`)}"`, "无 qop 时按两段式计算");
hasnt(noQop, "qop=", "无 qop 时不发 qop/nc/cnonce");

// ── 4. Digest 在 sendWithAuth 中的 401 重发 ──
function digestDraft() {
  return { ...emptyDraft("GET", "http://h/protected"), authType: "digest", authDigestUser: "u", authDigestPass: "p" };
}
{
  const sent = [];
  const send = async (payload) => {
    sent.push(payload);
    return sent.length === 1
      ? { status: 401, headers: [{ key: "WWW-Authenticate", value: 'Digest realm="r", nonce="n", qop="auth"' }], body: "" }
      : { status: 200, headers: [], body: "ok" };
  };
  const resp = await sendWithAuth(digestDraft(), send);
  eq(sent.length, 2, "401 后重发一次");
  eq(sent[0].headers.filter((h) => h.key === "Authorization").length, 0, "首发不带 Authorization");
  has(sent[1].headers.find((h) => h.key === "Authorization").value, "Digest username=\"u\"", "重发带 Digest 头");
  eq(resp.status, 200, "返回重发后的响应");
}
{
  // 401 但没有 Digest challenge：原样返回，不重试
  const sent = [];
  const send = async (payload) => {
    sent.push(payload);
    return { status: 401, headers: [{ key: "WWW-Authenticate", value: 'Basic realm="r"' }], body: "" };
  };
  const resp = await sendWithAuth(digestDraft(), send);
  eq(sent.length, 1, "无 Digest challenge 时不重试");
  eq(resp.status, 401, "原样返回 401");
}
{
  // 重发仍 401：只重试一次，不能无限循环
  const sent = [];
  const send = async (payload) => {
    sent.push(payload);
    return { status: 401, headers: [{ key: "WWW-Authenticate", value: 'Digest realm="r", nonce="n", qop="auth"' }], body: "" };
  };
  await sendWithAuth(digestDraft(), send);
  eq(sent.length, 2, "重发失败也只重试一次");
}

// ── 5. OAuth 2.0 客户端凭据 ──
function oauthDraft(patch = {}) {
  return {
    ...emptyDraft("GET", "http://api/data"),
    authType: "oauth2",
    authOauth2TokenUrl: "http://auth/token",
    authOauth2ClientId: "cid",
    authOauth2ClientSecret: "sec",
    authOauth2Scope: "read write",
    authOauth2ClientAuth: "header",
    ...patch,
  };
}
{
  clearTokenCache();
  const sent = [];
  const send = async (payload) => {
    sent.push(payload);
    return payload.url === "http://auth/token"
      ? { status: 200, headers: [], body: JSON.stringify({ access_token: "AT1", token_type: "bearer", expires_in: 3600 }) }
      : { status: 200, headers: [], body: "data" };
  };
  await sendWithAuth(oauthDraft(), send);
  eq(sent.length, 2, "先换 token 再发业务请求");
  eq(sent[0].method, "POST", "token 请求用 POST");
  has(sent[0].body, "grant_type=client_credentials", "token 请求体带 grant_type");
  has(sent[0].body, "scope=read%20write", "scope 已 URL 编码");
  has(sent[0].headers.find((h) => h.key === "Authorization").value, `Basic ${Buffer.from("cid:sec").toString("base64")}`, "凭据走 Basic 头");
  hasnt(sent[0].body, "client_secret", "凭据在头里时不重复放进表单体");
  eq(sent[1].headers.find((h) => h.key === "Authorization").value, "Bearer AT1", "业务请求带 Bearer（token_type 首字母归一）");

  // 同一配置再发一次：命中缓存，不再请求 token 端点
  await sendWithAuth(oauthDraft(), send);
  eq(sent.length, 3, "第二次发送复用缓存的 token");
  eq(sent[2].headers.find((h) => h.key === "Authorization").value, "Bearer AT1", "缓存的 token 生效");

  // 改了配置（换 scope）→ 缓存键不同，重新换取
  await sendWithAuth(oauthDraft({ authOauth2Scope: "admin" }), send);
  eq(sent.length, 5, "配置变化后重新换取 token");
}
{
  clearTokenCache();
  const sent = [];
  const send = async (payload) => {
    sent.push(payload);
    return payload.url === "http://auth/token"
      ? { status: 200, headers: [], body: JSON.stringify({ access_token: "AT2" }) }
      : { status: 200, headers: [], body: "data" };
  };
  await sendWithAuth(oauthDraft({ authOauth2ClientAuth: "body" }), send);
  has(sent[0].body, "client_id=cid", "凭据放表单体时带 client_id");
  has(sent[0].body, "client_secret=sec", "凭据放表单体时带 client_secret");
  eq(sent[0].headers.some((h) => h.key === "Authorization"), false, "凭据放表单体时不发 Basic 头");
  eq(sent[1].headers.find((h) => h.key === "Authorization").value, "Bearer AT2", "缺 token_type 时默认 Bearer");
}
{
  clearTokenCache();
  const send = async (payload) =>
    payload.url === "http://auth/token"
      ? { status: 401, headers: [], body: '{"error":"invalid_client"}' }
      : { status: 200, headers: [], body: "data" };
  let err = "";
  try {
    await sendWithAuth(oauthDraft(), send);
  } catch (e) {
    err = String(e.message || e);
  }
  has(err, "HTTP 401", "取 token 失败时抛出可读错误");
  has(err, "invalid_client", "错误信息带上服务端响应");
}
{
  clearTokenCache();
  let err = "";
  try {
    await sendWithAuth(oauthDraft({ authOauth2TokenUrl: "  " }), async () => ({ status: 200, headers: [], body: "{}" }));
  } catch (e) {
    err = String(e.message || e);
  }
  has(err, "Access Token URL", "未填 token 端点时给出明确提示");
}

// ── 6. 面板展示元信息 ──
eq(AUTH_TYPE_METAS.map((m) => m.label), ["No Auth", "Basic Auth", "Bearer Token", "API Key", "Digest Auth", "OAuth 2.0"], "认证方式显示名对齐主流工具");
eq(authPreview(emptyDraft()), null, "No Auth 无附加预览");
has(
  authPreview({ ...emptyDraft(), authType: "basic", authBasicUser: "u", authBasicPass: "p" }).code,
  `Basic ${Buffer.from("u:p").toString("base64")}`,
  "Basic 预览给出真实 base64",
);
eq(authPreview({ ...emptyDraft(), authType: "apikey", authApikeyIn: "query", authApikeyKey: "k", authApikeyValue: "v" }).label, "附加查询参数", "API Key 放 query 时预览标签随之变化");
for (const t of ["basic", "bearer", "apikey", "digest", "oauth2"]) {
  const p = authPreview({ ...emptyDraft(), authType: t });
  ok(p && !p.code.includes("\n"), `${t} 预览为单行`);
}

// ── 7. YAML 往返：新增的两种认证要能存能读 ──
{
  const yaml = `
apicase: "0.1"
steps:
  - id: s1
    protocol: http
    request:
      method: get
      url: http://h/x
      auth:
        type: digest
        digest: { username: u, password: p }
  - id: s2
    protocol: http
    request:
      method: get
      url: http://h/y
      auth:
        type: oauth2
        oauth2: { tokenUrl: "http://auth/token", clientId: cid, clientSecret: sec, scope: read, clientAuth: body }
`;
  const c = parseCase(yaml);
  eq(c.requests[0].http.auth, { type: "digest", digest: { username: "u", password: "p" } }, "解析 digest 认证");
  eq(
    c.requests[1].http.auth,
    { type: "oauth2", oauth2: { tokenUrl: "http://auth/token", clientId: "cid", clientSecret: "sec", scope: "read", clientAuth: "body" } },
    "解析 oauth2 认证",
  );
  const round = parseCase(dumpCase(c));
  eq(round.requests[0].http.auth, c.requests[0].http.auth, "digest 往返一致");
  eq(round.requests[1].http.auth, c.requests[1].http.auth, "oauth2 往返一致");
  // clientAuth=header 是默认值，不该落盘
  const c2 = parseCase(yaml.replace("clientAuth: body", "clientAuth: header"));
  hasnt(dumpCase(c2), "clientAuth", "默认的 clientAuth 不写进 YAML");
}

report();
