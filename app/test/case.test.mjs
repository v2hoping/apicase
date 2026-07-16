// case.ts 单元测试：steps/protocol/request/docs 解析、序列化、往返、旧格式拒绝。
import { loadModule, eq, ok, has, hasnt, report } from "./harness.mjs";

const { parseCase, dumpCase, analyzeCase } = await loadModule("src/case.ts");

// ── 1. 解析新格式 ──
const src = `
apicase: "0.1"
name: 登录并下单
vars:
  baseUrl: https://api.example.com
steps:
  - id: login
    protocol: http
    request:
      method: post
      url: "{{baseUrl}}/login"
      query:
        - { name: a, value: "1" }
        - { name: b, value: "2", enabled: false }
      body:
        type: json
        json: { user: admin }
    outputs:
      token: $.data.token
    assertions:
      - { target: status, op: eq, value: "200" }
    docs: |
      登录接口
  - id: createOrder
    protocol: http
    dependsOn: [login]
    request:
      method: POST
      url: "{{baseUrl}}/orders"
`;
const c = parseCase(src);
eq(c.requests.length, 2, "解析出 2 个 step");
eq(c.requests[0].id, "login", "step0 id");
eq(c.requests[0].protocol, "http", "step0 protocol");
eq(c.requests[0].http.method, "POST", "method 归一化为大写");
eq(c.requests[0].http.url, "{{baseUrl}}/login", "url 原样保留（变量不在解析期替换）");
eq(c.requests[0].http.query.length, 2, "query 两项");
eq(c.requests[0].http.query[1].enabled, false, "第二项 enabled=false 保留");
eq(c.requests[0].outputs, [{ name: "token", path: "$.data.token" }], "outputs map→数组");
eq(c.requests[0].assertions.length, 1, "assertions 一条");
ok(c.requests[0].docs && c.requests[0].docs.includes("登录接口"), "docs 解析");
eq(c.requests[1].dependsOn, ["login"], "createOrder 依赖 login");
eq(c.requests[1].protocol, "http", "step1 protocol 默认");

// ── 2. protocol 缺省为 http ──
const c2 = parseCase(`apicase: "0.1"\nsteps:\n  - id: x\n    request: { method: GET, url: http://a.com }\n`);
eq(c2.requests[0].protocol, "http", "缺 protocol 时默认 http");

// ── 3. 序列化：键序 id→protocol→dependsOn→request→outputs→assertions→docs ──
const dumped = dumpCase(c);
has(dumped, "steps:", "dump 顶层 steps");
has(dumped, "protocol: http", "dump 含 protocol");
has(dumped, "request:", "dump 报文键为 request");
hasnt(dumped, "requests:", "dump 不含旧 requests 键");
hasnt(dumped, "\n    http:", "dump 不含旧 http 报文键");
// id 行后紧跟 protocol 行
ok(/id: login\n\s*protocol: http/.test(dumped), "id 后紧跟 protocol");
// protocol 之后是 request（login 有 dependsOn? 无 → 直接 request）
ok(/protocol: http\n\s*request:/.test(dumped) || /protocol: http\n\s*dependsOn:/.test(dumped), "protocol 后是 request 或 dependsOn");

// ── 4. 往返稳定：parse(dump(parse(x))) 与 parse(x) 等价 ──
const c3 = parseCase(dumpCase(c));
eq(c3.requests.map((r) => [r.id, r.protocol, r.http.method]), c.requests.map((r) => [r.id, r.protocol, r.http.method]), "往返后 id/protocol/method 稳定");
eq(dumpCase(c3), dumpCase(c), "二次 dump 幂等");

// ── 5. analyzeCase：新格式有效，旧格式无效 ──
eq(analyzeCase(src).valid, true, "新格式 analyzeCase 有效");
const oldFmt = `apicase: "0.1"\nrequests:\n  - id: r1\n    http: { method: GET, url: http://a.com }\n`;
eq(analyzeCase(oldFmt).valid, false, "旧 requests 格式判为无效");
has(analyzeCase(oldFmt).error || "", "steps", "无效原因提示缺少 steps");
eq(analyzeCase(`foo: bar`).valid, false, "无 steps 的普通 yaml 无效");

// ── 6. 旧格式解析为空 steps（不再兼容）──
eq(parseCase(oldFmt).requests.length, 0, "旧 requests 解析出 0 个 step（被忽略）");

// ── 6b. 止损：steps 配旧内层 http: 键 → 判无效（防结构化保存覆盖丢报文）──
const halfMigrated = `apicase: "0.1"\nsteps:\n  - id: x\n    protocol: http\n    http: { method: POST, url: http://a.com/login }\n`;
eq(analyzeCase(halfMigrated).valid, false, "steps + 旧 http: 内层键判为无效");
has(analyzeCase(halfMigrated).error || "", "http", "错误提示指向 http: 键");
// 正常新格式（request:）不受影响
eq(analyzeCase(`apicase: "0.1"\nsteps:\n  - id: x\n    protocol: http\n    request: { method: GET, url: http://a.com }\n`).valid, true, "正常 request: 键仍有效");

// ── 7. docs 空则不落盘 ──
const cNoDocs = parseCase(`apicase: "0.1"\nsteps:\n  - id: x\n    protocol: http\n    request: { method: GET, url: http://a.com }\n`);
hasnt(dumpCase(cNoDocs), "docs:", "空 docs 不序列化");

// ── 8. dependsOn 空则不落盘 ──
hasnt(dumpCase(cNoDocs), "dependsOn:", "空 dependsOn 不序列化");

// ── 9. outputs 序列化为 map 形态（{变量名: JSONPath}），非 list ──
const cOut = parseCase(`apicase: "0.1"\nsteps:\n  - id: x\n    protocol: http\n    request: { method: GET, url: http://a.com }\n    outputs:\n      token: $.data.token\n`);
has(dumpCase(cOut), "outputs:", "有 outputs 则序列化");
ok(/outputs:\n\s*token: \$\.data\.token/.test(dumpCase(cOut)), "outputs 为 map 形态 token: $.data.token");
hasnt(dumpCase(cOut), "- name: token", "outputs 不是 list 形态");

// ── 10. docs 往返稳定 ──
const cDoc = parseCase(`apicase: "0.1"\nsteps:\n  - id: x\n    protocol: http\n    request: { method: GET, url: http://a.com }\n    docs: |\n      # 标题\n      正文一行\n`);
has(cDoc.requests[0].docs, "# 标题", "docs 多行解析");
const cDoc2 = parseCase(dumpCase(cDoc));
eq(cDoc2.requests[0].docs, cDoc.requests[0].docs, "docs 往返稳定");

report();
