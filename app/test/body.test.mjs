// 请求体单元测试：draft ↔ HttpSpec 互转、发送载荷组装（Content-Type 默认值 /
// binary 走 bodyFile / form-data 走 formData）、以及 xml|binary 的 YAML 往返。
import { loadModule, eq, ok, has, hasnt, report } from "./harness.mjs";

const { emptyDraft, buildApiRequest, draftToRequest, requestToDraft, DEFAULT_CONTENT_TYPE, guessContentType, baseName } =
  await loadModule("src/draft.ts");
const { parseCase, dumpCase } = await loadModule("src/case.ts");

const ct = (p) => p.headers.find((h) => h.key.toLowerCase() === "content-type")?.value;
const draft = (patch) => ({ ...emptyDraft("POST", "http://h/x"), ...patch });

// ── 1. 文本类请求体的默认 Content-Type ──
{
  const p = buildApiRequest(draft({ bodyType: "json", bodyText: '{"a":1}' }));
  eq(p.body, '{"a":1}', "json 体原样发送");
  eq(ct(p), "application/json; charset=utf-8", "json 默认 Content-Type 带 charset");
}
{
  const p = buildApiRequest(draft({ bodyType: "xml", bodyText: "<a/>" }));
  eq(p.body, "<a/>", "xml 体原样发送");
  eq(ct(p), "application/xml; charset=utf-8", "xml 默认 Content-Type 带 charset");
}
{
  const p = buildApiRequest(draft({ bodyType: "text", bodyText: "hello" }));
  eq(ct(p), "text/plain; charset=utf-8", "text 未填 Content-Type 时用 text/plain; charset=utf-8");
}
{
  const p = buildApiRequest(draft({ bodyType: "text", bodyText: "hello", bodyContentType: "text/csv" }));
  eq(ct(p), "text/csv", "text 填了 Content-Type 则用填写值");
}
{
  // 手填的 Content-Type 请求头优先级最高，不应被默认值顶掉，也不应重复
  const p = buildApiRequest(
    draft({ bodyType: "json", bodyText: "{}", headers: [{ name: "Content-Type", value: "application/vnd.api+json", enabled: true }] }),
  );
  eq(ct(p), "application/vnd.api+json", "手填 Content-Type 覆盖默认值");
  eq(p.headers.filter((h) => h.key.toLowerCase() === "content-type").length, 1, "Content-Type 不重复");
}
{
  const p = buildApiRequest(draft({ bodyType: "json", bodyText: "   " }));
  eq(p.body, null, "空 json 文本不发 body");
  eq(ct(p), undefined, "空 json 文本不加 Content-Type");
}

// ── 2. 表单 ──
{
  const rows = [
    { name: "a", value: "1", enabled: true },
    { name: "b", value: "两 个", enabled: true },
    { name: "c", value: "3", enabled: false },
    { name: "", value: "x", enabled: true },
  ];
  const p = buildApiRequest(draft({ bodyType: "form-urlencoded", bodyForm: rows }));
  eq(p.body, "a=1&b=%E4%B8%A4%20%E4%B8%AA", "form-urlencoded 编码并跳过禁用/空名行");
  eq(ct(p), "application/x-www-form-urlencoded", "form-urlencoded 默认 Content-Type");

  const m = buildApiRequest(draft({ bodyType: "form-data", bodyForm: rows }));
  eq(m.body, null, "form-data 不走 body 字符串");
  eq(m.formData, [{ name: "a", value: "1" }, { name: "b", value: "两 个" }], "form-data 传字段列表给后端组 multipart");
  eq(ct(m), undefined, "form-data 不由前端设 Content-Type（boundary 归后端）");
}
{
  const p = buildApiRequest(draft({ bodyType: "form-data", bodyForm: [] }));
  eq(p.formData, undefined, "form-data 无字段时不带 formData");
}

// ── 2.1 form-data 的文件字段（multipart 上传）──
{
  const rows = [
    { name: "title", value: "头像", enabled: true },
    { name: "file", value: "/tmp/pics/avatar.png", type: "file", enabled: true },
    { name: "skip", value: "/tmp/x.png", type: "file", enabled: false }, // 禁用行
    { name: "empty", value: "  ", type: "file", enabled: true }, // 文件行未选文件
  ];
  const p = buildApiRequest(draft({ bodyType: "form-data", bodyForm: rows }));
  eq(
    p.formData,
    [
      { name: "title", value: "头像" },
      { name: "file", filePath: "/tmp/pics/avatar.png", fileName: "avatar.png", contentType: "image/png" },
    ],
    "文件字段带 filePath/fileName/contentType，禁用行与未选文件的行被跳过",
  );
  eq(ct(p), undefined, "带文件的 form-data 仍不由前端设 Content-Type");
  eq(p.body, null, "带文件的 form-data 不走 body 字符串");
}
{
  // 文本行与文件行只在 type 上不同；文件行的 Content-Type 按扩展名走同一张表（推不出兜底 octet-stream）
  const p = buildApiRequest(
    draft({ bodyType: "form-data", bodyForm: [{ name: "f", value: "C:\\data\\报表.dat", type: "file", enabled: true }] }),
  );
  eq(p.formData, [{ name: "f", filePath: "C:\\data\\报表.dat", fileName: "报表.dat", contentType: "application/octet-stream" }], "Windows 路径按 \\ 取文件名");
}
{
  const p = buildApiRequest(draft({ bodyType: "form-data", bodyForm: [{ name: "f", value: "  ", type: "file", enabled: true }] }));
  eq(p.formData, undefined, "只有未选文件的文件行 → 不发 multipart");
}
// baseName 直接单测
eq(baseName("/a/b/c.png"), "c.png", "取 POSIX 路径文件名");
eq(baseName("C:\\a\\b.txt"), "b.txt", "取 Windows 路径文件名");
eq(baseName("plain.txt"), "plain.txt", "无目录时原样返回");
eq(baseName("  /a/b/c.png  "), "c.png", "去两端空白");

// ── 3. binary（Content-Type 对齐 Postman：按扩展名推断，兜底 octet-stream，手填覆盖）──
{
  const p = buildApiRequest(draft({ bodyType: "binary", bodyFilePath: " /tmp/payload.bin " }));
  eq(p.bodyFile, "/tmp/payload.bin", "binary 传文件路径（去空白）给后端读盘");
  eq(p.body, null, "binary 不走 body 字符串");
  eq(ct(p), "application/octet-stream", "binary 未知扩展名兜底 octet-stream");

  eq(ct(buildApiRequest(draft({ bodyType: "binary", bodyFilePath: "/tmp/a.png" }))), "image/png", "binary 按 .png 推断 image/png");
  eq(ct(buildApiRequest(draft({ bodyType: "binary", bodyFilePath: "/tmp/DOC.PDF" }))), "application/pdf", "扩展名大小写不敏感");
  eq(ct(buildApiRequest(draft({ bodyType: "binary", bodyFilePath: "/tmp/noext" }))), "application/octet-stream", "无扩展名兜底 octet-stream");

  const withCt = buildApiRequest(draft({ bodyType: "binary", bodyFilePath: "/tmp/a.png", bodyContentType: "application/custom" }));
  eq(ct(withCt), "application/custom", "binary 手填 Content-Type 覆盖推断");
}
{
  const p = buildApiRequest(draft({ bodyType: "binary", bodyFilePath: "  " }));
  eq(p.bodyFile, undefined, "binary 未选文件时不带 bodyFile");
  eq(ct(p), undefined, "binary 未选文件时不发 Content-Type");
}
// guessContentType 直接单测
eq(guessContentType("photo.jpeg"), "image/jpeg", "jpeg → image/jpeg");
eq(guessContentType("/a/b/data.json"), "application/json", "带目录的路径也按扩展名");
eq(guessContentType("archive.unknownext"), "application/octet-stream", "未知扩展名兜底");
eq(guessContentType("Makefile"), "application/octet-stream", "无扩展名兜底");

// ── 4. draft ↔ HttpSpec 互转 ──
{
  const { request, error } = draftToRequest(draft({ bodyType: "xml", bodyText: "<a/>" }));
  eq(error, undefined, "xml 草稿可保存");
  eq(request.body, { type: "xml", xml: "<a/>" }, "xml 存进同名子键");
  eq(requestToDraft(request).bodyText, "<a/>", "xml 读回编辑文本");
}
{
  const { request } = draftToRequest(draft({ bodyType: "binary", bodyFilePath: "./p.bin", bodyContentType: "image/png" }));
  eq(request.body, { type: "binary", filePath: "./p.bin", contentType: "image/png" }, "binary 存路径与 Content-Type");
  const back = requestToDraft(request);
  eq(back.bodyFilePath, "./p.bin", "binary 读回文件路径");
  eq(back.bodyContentType, "image/png", "binary 读回 Content-Type");
}
{
  const rows = [
    { name: "title", value: "头像", enabled: true },
    { name: "file", value: "./avatar.png", type: "file", enabled: true },
  ];
  const { request } = draftToRequest(draft({ bodyType: "form-data", bodyForm: rows }));
  eq(request.body, { type: "form-data", formData: rows }, "form-data 草稿保留每行的 type");
  eq(requestToDraft(request).bodyForm, rows, "form-data 读回时文件行仍是 file");
}
eq(DEFAULT_CONTENT_TYPE["form-data"], undefined, "form-data 没有前端默认 Content-Type");

// ── 5. YAML 往返 ──
{
  const yaml = `
apicase: "0.1"
steps:
  - id: s1
    protocol: http
    request:
      method: post
      url: http://h/x
      body: { type: xml, xml: "<a/>" }
  - id: s2
    protocol: http
    request:
      method: post
      url: http://h/y
      body: { type: binary, contentType: image/png, filePath: ./avatar.png }
`;
  const c = parseCase(yaml);
  eq(c.requests[0].http.body, { type: "xml", xml: "<a/>" }, "解析 xml 体");
  eq(c.requests[1].http.body, { type: "binary", filePath: "./avatar.png", contentType: "image/png" }, "解析 binary 体");
  const round = parseCase(dumpCase(c));
  eq(round.requests[0].http.body, c.requests[0].http.body, "xml 往返一致");
  eq(round.requests[1].http.body, c.requests[1].http.body, "binary 往返一致");

  const dumped = dumpCase(c);
  has(dumped, "type: binary", "binary 类型落盘");
  // 空内容的体不落盘
  const empty = parseCase(yaml.replace('xml: "<a/>"', 'xml: ""').replace("filePath: ./avatar.png", 'filePath: ""'));
  hasnt(dumpCase(empty), "type: xml", "空 xml 体不落盘");
  hasnt(dumpCase(empty), "type: binary", "未选文件的 binary 体不落盘");
}
{
  // form-data：文件行落 `type: file`，文本行不落 type（text 是默认值）
  const yaml = `
apicase: "0.1"
steps:
  - id: s1
    protocol: http
    request:
      method: post
      url: http://h/upload
      body:
        type: form-data
        formData:
          - { name: title, value: 头像 }
          - { name: file, type: file, value: ./avatar.png, description: 待上传 }
          - { name: legacy, type: text, value: "1" }
`;
  const c = parseCase(yaml);
  eq(
    c.requests[0].http.body,
    {
      type: "form-data",
      formData: [
        { name: "title", value: "头像", enabled: true, description: undefined },
        { name: "file", value: "./avatar.png", enabled: true, description: "待上传", type: "file" },
        { name: "legacy", value: "1", enabled: true, description: undefined },
      ],
    },
    "解析 form-data 的文件字段；显式 type: text 归一为不带 type",
  );
  const dumped = dumpCase(c);
  has(dumped, "type: file", "文件行落盘 type: file");
  eq((dumped.match(/type: text/g) || []).length, 0, "文本行不落 type（默认值裁剪）");
  eq(parseCase(dumped).requests[0].http.body, c.requests[0].http.body, "form-data 文件字段往返一致");
}

report();
