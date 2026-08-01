// 编辑态 ↔ HttpSpec 互转 —— 改造后前端在请求这条链路上的**全部**职责。
//
// 「HttpSpec → 真正发出去的报文」（认证头、Content-Type、multipart 组装）已下沉 Rust，
// 由 core/src/request.rs 的单测覆盖。这里测的是纯 UI 关注点：
// 表单控件读得到值吗？改完能原样存回去吗？JSON 写坏了会不会被静默吞掉？
import { loadModule, eq, ok, report } from "./harness.mjs";

const { emptyDraft, requestToDraft, draftToRequest, caseToRequests, guessContentType } = await loadModule("src/draft.ts");

// ── 空草稿 ──────────────────────────────────────────

const empty = emptyDraft();
eq(empty.method, "GET", "默认方法");
eq(empty.authType, "none", "默认不带认证");
eq(empty.bodyType, "none", "默认无报文体");
eq(emptyDraft("POST", "http://x?a=1").query, [{ name: "a", value: "1", enabled: true }], "URL 里的 query 自动拆进表格");

// ── HttpSpec → 草稿 ─────────────────────────────────

const spec = {
  method: "POST",
  url: "http://x/api",
  query: [{ name: "q", value: "1", enabled: true }],
  headers: [{ name: "X-A", value: "v", enabled: true }],
  auth: { type: "oauth2", oauth2: { tokenUrl: "http://t", clientId: "c", clientSecret: "s", scope: "read", clientAuth: "body" } },
  body: { type: "json", json: { a: 1, b: "x" } },
};
const d = requestToDraft(spec);
eq(d.method, "POST", "方法透传");
eq(d.url, "http://x/api?q=1", "query 合并进 URL（输入框与表格双向绑定）");
eq(d.authOauth2ClientAuth, "body", "认证子字段摊平到草稿");
eq(d.bodyText, '{\n  "a": 1,\n  "b": "x"\n}', "JSON 报文体以缩进文本形态进编辑器");

// ── 草稿 → HttpSpec ─────────────────────────────────

const back = draftToRequest(d);
ok(!back.error, "合法草稿不该报错");
eq(back.request.url, "http://x/api", "存回时 URL 只留 base（query 单列，避免两处重复）");
eq(back.request.query, [{ name: "q", value: "1", enabled: true }], "query 从表格取");
eq(back.request.auth.oauth2.scope, "read", "认证子字段还原");
eq(back.request.body.json, { a: 1, b: "x" }, "JSON 报文体还原为结构（diff 友好，不是字符串）");

// 往返：结构不该在编辑器里转一圈就变样
eq(draftToRequest(requestToDraft(spec)).request, back.request, "spec → 草稿 → spec 往返一致");

// ── JSON 写坏了要挡住，不能静默吞掉 ─────────────────
//
// 编辑 JSON 时允许中途非法（不即时 parse），但**保存边界必须校验**——
// 否则一次保存就把用户的报文体变成了 `{}`。

const bad = draftToRequest({ ...empty, bodyType: "json", bodyText: "{ 这不是 JSON" });
ok(!!bad.error, "非法 JSON 必须报错");
ok(!bad.request, "报错时不该产出半成品 spec");
ok(bad.error.includes("JSON"), `错误信息应指明是 JSON 问题：${bad.error}`);

// 空 JSON 文本 = 没有报文体（而不是一个 `type: json` 的空壳）
eq(draftToRequest({ ...empty, bodyType: "json", bodyText: "   " }).request.body, { type: "none" }, "空 JSON 文本视作无报文体");

// ── 各报文体类型往返 ────────────────────────────────

const bodies = [
  { type: "xml", xml: "<a/>" },
  { type: "text", text: "hello", contentType: "text/csv" },
  { type: "binary", filePath: "/p/a.png", contentType: "image/png" },
  { type: "form-urlencoded", urlencoded: [{ name: "a", value: "1", enabled: true }] },
  { type: "form-data", formData: [{ name: "f", value: "/p/a.png", enabled: true, type: "file" }] },
];
for (const body of bodies) {
  const s = { ...spec, auth: { type: "none" }, body };
  eq(draftToRequest(requestToDraft(s)).request.body, body, `${body.type} 往返一致`);
}

// ── case → step 列表 ────────────────────────────────

const { requests } = caseToRequests({
  version: "0.1",
  requests: [
    { id: "a", protocol: "http", http: spec, dependsOn: [], outputs: [{ name: "t", path: "$.t" }], assertions: [] },
  ],
});
eq(requests.length, 1, "步骤数");
eq(requests[0].id, "a", "id 透传");
eq(requests[0].outputs, [{ name: "t", path: "$.t" }], "输出提取配置透传");

// 空 steps 兜一个空请求，保证编辑器恒有内容可渲染（否则打开新建的 case 是一片空白）
const fallback = caseToRequests({ version: "0.1", requests: [] });
eq(fallback.requests.length, 1, "空 case 兜底出一个空请求");
eq(fallback.requests[0].id, "step1", "兜底请求的 id");

// 坐标在文件里跟着 step 走，进了前端收成一张 id → 坐标表（画布按 id 查最顺手）
const step = (id, ui) => ({ id, protocol: "http", http: spec, dependsOn: [], outputs: [], assertions: [], ui });
eq(
  caseToRequests({ version: "0.1", requests: [step("a", { x: 1, y: 2 }), step("b", { x: 3, y: 4 })] }).ui,
  { a: { x: 1, y: 2 }, b: { x: 3, y: 4 } },
  "各 step 的坐标收成 id → 坐标表",
);
eq(
  caseToRequests({ version: "0.1", requests: [step("a", { x: 1, y: 2 }), step("b", undefined)] }).ui,
  { a: { x: 1, y: 2 } },
  "只有一部分 step 有坐标时，其余交给自动布局",
);
eq(caseToRequests({ version: "0.1", requests: [step("a", undefined)] }).ui, undefined, "一个坐标都没有时给 undefined 而非 {}");

// ── 扩展名 → MIME（binary 选文件后自动定 Content-Type）──

eq(guessContentType("a.png"), "image/png", "按扩展名推断");
eq(guessContentType("a.JSON"), "application/json", "扩展名大小写不敏感");
eq(guessContentType("noext"), "application/octet-stream", "推不出时兜底");

report();
