use super::*;

/// 用户实际仓库里的一个 case，逐字比对输出。
/// 这是本模块的黄金测试——格式是用户要读、要 diff 的东西，跑偏了必须立刻被发现。
const GOLDEN: &str = r#"apicase: v0.1
name: GET 回显参数
steps:
  - id: get
    protocol: http
    ui:
      x: 502
      y: -70
    request:
      method: GET
      url: ${{baseUrl}}/get
      query:
        - name: foo
          value: bar
        - name: page
          value: '2'
    assertions:
      - target: res.status
        op: eq
        value: 200
      - target: res.body.args.foo
        op: eq
        value: bar
      - target: res.body.args.page
        op: eq
        value: 2
    docs: |
      httpbin `/get`：把 URL 的 query 参数原样回显到响应体 `$.args`。
      在「参数」页可看到 foo / page 两个查询参数（与 URL 双向同步）。
"#;

/// 读进来再写出去，逐字不变。既钉住解析的完整性，也钉住输出格式。
#[test]
fn golden_case_roundtrips_verbatim() {
    let c = parse_case(GOLDEN).expect("应能解析");
    assert_eq!(dump_case(&c), GOLDEN);
}

/// 旧的 js-yaml 输出会写成 `'y': -70`（YAML 1.1 把 y 当布尔别名）。
/// 新输出按 YAML 1.2 裸写——这正是用户指出的那处不统一。
#[test]
fn ui_coordinate_key_y_is_not_quoted() {
    let c = parse_case(GOLDEN).expect("应能解析");
    let out = dump_case(&c);
    assert!(out.contains("      y: -70\n"), "y 不该带引号：\n{out}");
    assert!(!out.contains("'y'"), "输出里不该再出现 'y'：\n{out}");
    // 而 1.1 的引号写法仍要**读得进来**——仓库里已有的文件就是那么写的
    let legacy = GOLDEN.replace("y: -70", "'y': -70");
    let c2 = parse_case(&legacy).expect("旧写法应仍能解析");
    assert_eq!(c2.requests[0].ui, c.requests[0].ui, "两种写法解析结果必须一致");
}

/// 坐标挂在 step 上，不再是与 `steps:` 并行的 id 映射表
#[test]
fn step_ui_replaces_the_top_level_node_table() {
    let c = parse_case(GOLDEN).expect("应能解析");
    assert_eq!(c.requests[0].ui, Some(StepUi { x: 502.0, y: -70.0 }));
    // 顶层 ui: 块是旧格式，硬切换后不再识别（坐标丢失 → 回退自动布局）
    let legacy = "apicase: v0.1\nsteps:\n  - id: a\n    protocol: http\nui:\n  nodes:\n    a:\n      x: 1\n      y: 2\n";
    let c2 = parse_case(legacy).expect("应能解析");
    assert_eq!(c2.requests[0].ui, None, "顶层 ui.nodes 不再被读取");
    assert!(!dump_case(&c2).contains("ui:"), "也不该再写出来");
}

/// 坐标写坏了只丢坐标，不该废掉整条用例
#[test]
fn broken_coordinates_fall_back_to_auto_layout() {
    for bad in ["ui: { x: 一, y: 2 }", "ui: { x: 1 }", "ui: hello", "ui: {}"] {
        let text = format!("apicase: v0.1\nsteps:\n  - id: a\n    protocol: http\n    {bad}\n");
        let c = parse_case(&text).unwrap_or_else(|e| panic!("{bad} 不该让解析失败：{e}"));
        assert_eq!(c.requests[0].ui, None, "{bad} 应回退为无坐标");
        assert_eq!(c.requests[0].id, "a", "{bad} 不该影响 step 本身");
    }
}

fn sample_case() -> Case {
    parse_case(
        r#"
apicase: v0.1
name: 登录后取用户
vars:
  base: http://localhost
  retries: 3
steps:
  - id: login
    protocol: http
    request:
      method: POST
      url: '{{base}}/login'
      headers:
        - name: X-Trace
          value: t1
          description: 链路追踪
        - name: X-Off
          value: v
          enabled: false
      auth:
        type: basic
        basic:
          username: u
          password: p
      body:
        type: json
        json:
          user: alice
          pwd: '123'
    outputs:
      token: res.body.data.token
    assertions:
      - target: res.status
        op: eq
        value: '200'
  - id: profile
    protocol: http
    dependsOn:
      - login
    request:
      method: GET
      url: '{{base}}/me'
      auth:
        type: bearer
        bearer:
          token: '{{steps.login.outputs.token}}'
    assertions:
      - target: res.body.data.name
        op: exists
"#,
    )
    .expect("应能解析")
}

#[test]
fn parses_structure() {
    let c = sample_case();
    assert_eq!(c.version, "v0.1");
    assert_eq!(c.name.as_deref(), Some("登录后取用户"));
    assert_eq!(c.requests.len(), 2);

    let login = &c.requests[0];
    assert_eq!(login.id, "login");
    assert_eq!(login.http.method, "POST");
    assert_eq!(login.http.headers.len(), 2);
    assert!(login.http.headers[0].enabled);
    assert_eq!(login.http.headers[0].description.as_deref(), Some("链路追踪"));
    assert!(!login.http.headers[1].enabled, "enabled: false 应被读到");
    assert_eq!(login.http.auth.kind, AuthType::Basic);
    assert_eq!(login.http.auth.basic.as_ref().unwrap().username, "u");
    assert_eq!(login.outputs, vec![StepOutput { name: "token".into(), path: "res.body.data.token".into() }]);
    // YAML 里写的是数字 200，模型里恒是字符串
    assert_eq!(login.assertions[0].value.as_deref(), Some("200"));

    let profile = &c.requests[1];
    assert_eq!(profile.depends_on, vec!["login"]);
    assert_eq!(profile.assertions[0].op, AssertOp::Exists);
    assert_eq!(profile.assertions[0].value, None);

    // vars 保留原始类型（数字仍是数字），书写顺序也保住
    let vars = c.vars.as_ref().unwrap();
    assert_eq!(vars.get("retries"), Some(&serde_json::json!(3)));
    assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["base", "retries"]);
}

/// **宽进严出**：手写时可以不加引号（解析侧一律走 `s()` 转字符串），
/// 保存时规范化成带引号——把"这是个字符串"这件事在文件里写明白。
#[test]
fn bare_scalars_are_read_then_normalized() {
    let c = parse_case(
        r#"
apicase: 0.1
name: 123
steps:
  - id: 42
    protocol: http
    request:
      method: GET
      url: http://x/a
      query:
        - name: page
          value: 2
          description: 456
        - name: 1
          value: 1
        - name: flag
          value: true
      headers:
        - name: X-Count
          value: 7
    assertions:
      - target: res.status
        op: eq
        value: 200
"#,
    )
    .expect("应能解析");
    // 解析侧宽容：数字 / 布尔裸写都读成字符串。
    // 这里输入的是老写法 `apicase: 0.1`（YAML 读成数字 0.1），读出来是字符串 "0.1"
    assert_eq!(c.version, "0.1");
    assert_eq!(c.name.as_deref(), Some("123"));
    let st = &c.requests[0];
    assert_eq!(st.id, "42");
    assert_eq!(st.http.query[0].value, "2");
    assert_eq!(st.http.query[0].description.as_deref(), Some("456"));
    // KV 的 name 也可能是纯数字——它在 YAML 里是**值位置**（`name:` 才是 key），
    // 所以同样被读成字符串。这正是「数组 + name/value」优于「map 形式」的地方。
    assert_eq!(st.http.query[1].name, "1");
    assert_eq!(st.http.query[1].value, "1");
    assert_eq!(st.http.query[2].value, "true");
    assert_eq!(st.http.headers[0].value, "7");
    assert_eq!(st.assertions[0].value.as_deref(), Some("200"));

    // 写回时规范化：字符串值一律带引号，读者一眼看出类型
    let out = dump_case(&c);
    // "0.1" 是数字形态的字符串 → 写回时带引号。想去掉这个引号得把值本身改成 `v0.1`
    // （这也是新建 case 的默认值：见 CASE_VERSION）。
    // 注意 `assertions[].value` 不在其列——它走宽松规则，`value: 200` 裸写。
    for want in ["apicase: '0.1'", "name: '123'", "id: '42'", "value: '2'", "name: '1'", "value: 'true'"] {
        assert!(out.contains(want), "输出应含 {want}：\n{out}");
    }
    // 再写一次不再变化（幂等）
    let c2 = parse_case(&out).expect("重解析");
    assert_eq!(c2, c);
    assert_eq!(dump_case(&c2), out, "二次序列化应完全一致");
    // 断言的期望值走宽松规则：不进报文、类型跟着 target 走
    assert!(out.contains("        value: 200\n"), "断言期望值该裸写：\n{out}");
}

/// `vars` 与 `body.json` 里类型由用户决定，**必须原样保住**：
/// `retries: 3` 是数字、`retries: '3'` 是字符串，两者语义不同。
#[test]
fn free_subtree_keeps_user_types() {
    let text = r#"apicase: v0.1
vars:
  num: 3
  str: '3'
  yes_str: yes
  flag: true
  flag_str: 'true'
steps:
  - id: a
    protocol: http
    request:
      method: POST
      url: http://x
      body:
        type: json
        json:
          n: 1
          s: '1'
          b: false
          bs: 'false'
"#;
    let c = parse_case(text).expect("应能解析");
    let vars = c.vars.as_ref().unwrap();
    assert_eq!(vars["num"], serde_json::json!(3), "数字保持数字");
    assert_eq!(vars["str"], serde_json::json!("3"), "带引号的仍是字符串");
    assert_eq!(vars["yes_str"], serde_json::json!("yes"), "yes 按 1.2 读成字符串");
    assert_eq!(vars["flag"], serde_json::json!(true));
    assert_eq!(vars["flag_str"], serde_json::json!("true"));

    let json = c.requests[0].http.body.json.as_ref().unwrap();
    assert_eq!(json["n"], serde_json::json!(1));
    assert_eq!(json["s"], serde_json::json!("1"));
    assert_eq!(json["b"], serde_json::json!(false));
    assert_eq!(json["bs"], serde_json::json!("false"));

    // 逐字往返：类型区分靠的正是那几个引号，一个都不能少、也不该多
    assert_eq!(dump_case(&c), text);
}

/// 解析 → 序列化 → 再解析，模型完全相同（幂等）。
#[test]
fn parse_dump_parse_is_idempotent() {
    let c = sample_case();
    let text = dump_case(&c);
    let c2 = parse_case(&text).expect("重解析应成功");
    assert_eq!(c, c2);
    assert_eq!(dump_case(&c2), text, "二次序列化应完全一致");
}

/// 手写文件里的错误取值必须回落默认，而不是让整份 case 解析失败——
/// 那样用户连哪一行写错了都看不到。
#[test]
fn malformed_values_fall_back_instead_of_failing() {
    let c = parse_case(
        r#"
apicase: v0.1
steps:
  - id: a
    request:
      method: get
      url: http://x
      auth:
        type: 我不认识
      body:
        type: 也不认识
    assertions:
      - target: res.status
        op: equals
        value: '1'
      - op: eq
        value: '2'
    dependsOn: 不是数组
    outputs: 也不是对象
"#,
    )
    .expect("不该失败");
    let st = &c.requests[0];
    assert_eq!(st.http.method, "GET", "方法统一大写");
    assert_eq!(st.protocol, "http", "protocol 缺省补 http");
    assert_eq!(st.http.auth.kind, AuthType::None);
    assert_eq!(st.http.body.kind, BodyType::None);
    assert_eq!(st.assertions.len(), 1, "没有 target 的断言被丢弃");
    assert_eq!(st.assertions[0].op, AssertOp::Eq, "认不出的 op 回落 eq");
    assert!(st.depends_on.is_empty());
    assert!(st.outputs.is_empty());
}

/// 缺 id 的 step 按位置补号（与前端此前行为一致）
#[test]
fn missing_step_id_is_numbered() {
    let c = parse_case("apicase: v0.1\nsteps:\n  - request:\n      url: http://a\n  - request:\n      url: http://b\n")
        .expect("应能解析");
    assert_eq!(c.requests[0].id, "step1");
    assert_eq!(c.requests[1].id, "step2");
}

#[test]
fn analyze_rejects_non_cases() {
    assert!(!analyze_case("这不是: [有效\n  yaml").valid, "语法错");
    assert!(!analyze_case("- 1\n- 2\n").valid, "顶层是数组");
    assert!(!analyze_case("name: x\n").valid, "没有 steps");

    let r = analyze_case("apicase: v0.1\nsteps:\n  - id: a\n    http:\n      url: http://x\n");
    assert!(!r.valid, "旧的 http: 报文键应判无效");
    assert!(r.error.unwrap().contains("http:"), "错误应指明原因");

    let r = analyze_case("apicase: v0.1\nsteps: []\n");
    assert!(r.valid, "空 steps 是有效 case（新建时就是这样）");
    assert_eq!(r.case.unwrap().requests.len(), 0);
}

/// 空文档不该报错——新建的 case 文件就是空的
#[test]
fn empty_document_is_handled() {
    assert!(!analyze_case("").valid);
    assert_eq!(parse_case("").expect("空文档应能解析").requests.len(), 0);
    assert_eq!(parse_case("# 只有注释\n").expect("纯注释应能解析").version, "v0.1");
}

#[test]
fn body_kinds_roundtrip() {
    let cases = [
        ("json", "      body:\n        type: json\n        json:\n          a: 1\n"),
        ("xml", "      body:\n        type: xml\n        xml: <a/>\n"),
        ("text", "      body:\n        type: text\n        contentType: text/csv\n        text: a,b\n"),
        ("binary", "      body:\n        type: binary\n        contentType: image/png\n        filePath: /p/a.png\n"),
        (
            "form-urlencoded",
            "      body:\n        type: form-urlencoded\n        urlencoded:\n          - name: a\n            value: '1'\n",
        ),
        (
            "form-data",
            "      body:\n        type: form-data\n        formData:\n          - name: f\n            type: file\n            value: /p/a.png\n          - name: t\n            value: v\n",
        ),
    ];
    for (kind, body) in cases {
        let text = format!("apicase: v0.1\nsteps:\n  - id: a\n    protocol: http\n    request:\n      method: POST\n      url: http://x\n{body}");
        let c = parse_case(&text).unwrap_or_else(|e| panic!("{kind} 应能解析: {e}"));
        assert_eq!(c.requests[0].http.body.kind, BodyType::from(kind));
        assert_eq!(dump_case(&c), text, "{kind} 往返应逐字一致");
    }
}

#[test]
fn auth_kinds_roundtrip() {
    let variants = [
        "      auth:\n        type: bearer\n        bearer:\n          token: t\n",
        "      auth:\n        type: basic\n        basic:\n          username: u\n          password: p\n",
        "      auth:\n        type: apikey\n        apikey:\n          key: k\n          value: v\n          in: query\n",
        "      auth:\n        type: digest\n        digest:\n          username: u\n          password: p\n",
        "      auth:\n        type: oauth2\n        oauth2:\n          tokenUrl: http://t\n          clientId: c\n          clientSecret: s\n          scope: read\n          clientAuth: body\n",
    ];
    for auth in variants {
        let text = format!("apicase: v0.1\nsteps:\n  - id: a\n    protocol: http\n    request:\n      method: GET\n      url: http://x\n{auth}");
        let c = parse_case(&text).expect("应能解析");
        assert_eq!(dump_case(&c), text, "auth 往返应逐字一致");
    }
    // header 与 scope 缺省不落盘
    let text = "apicase: v0.1\nsteps:\n  - id: a\n    protocol: http\n    request:\n      method: GET\n      url: http://x\n      auth:\n        type: oauth2\n        oauth2:\n          tokenUrl: http://t\n          clientId: c\n          clientSecret: s\n";
    let c = parse_case(text).expect("应能解析");
    assert_eq!(dump_case(&c), text);
}

/// 默认值不落盘：`enabled: true`、空 body、空列表都不该写进文件
#[test]
fn defaults_are_trimmed_on_dump() {
    let mut c = Case { version: CASE_VERSION.into(), ..Default::default() };
    c.requests.push(Step {
        id: "a".into(),
        protocol: "http".into(),
        ui: None,
        http: HttpSpec {
            method: "GET".into(),
            url: "http://x".into(),
            headers: vec![Kv::new("H", "v"), Kv { name: "  ".into(), value: "  ".into(), enabled: true, description: None }],
            ..Default::default()
        },
        depends_on: vec![],
        outputs: vec![],
        assertions: vec![],
        docs: None,
    });
    let out = dump_case(&c);
    assert_eq!(
        out,
        "apicase: v0.1\nsteps:\n  - id: a\n    protocol: http\n    request:\n      method: GET\n      url: http://x\n      headers:\n        - name: H\n          value: v\n"
    );
    assert!(!out.contains("enabled"), "enabled: true 不落盘");
    assert!(!out.contains("dependsOn"), "空 dependsOn 不落盘");
}

/// exists / notExists 没有期望值，写进去也不该落盘
#[test]
fn valueless_assert_ops_drop_their_value() {
    let mut c = Case { version: CASE_VERSION.into(), ..Default::default() };
    c.requests.push(Step {
        id: "a".into(),
        protocol: "http".into(),
        ui: None,
        http: HttpSpec { url: "http://x".into(), ..Default::default() },
        depends_on: vec![],
        outputs: vec![],
        assertions: vec![Assertion { target: "res.body.a".into(), op: AssertOp::Exists, value: Some("被忽略".into()) }],
        docs: None,
    });
    let out = dump_case(&c);
    assert!(out.contains("op: exists\n"), "{out}");
    assert!(!out.contains("被忽略"), "exists 不该落 value：{out}");
}

// ── application.yml ─────────────────────────────────

const APP_YML: &str = r#"# apicase 工作空间配置
environment:
  dev:
    baseUrl: http://localhost:8080
    retries: 3
  prod:
    baseUrl: https://api.example.com
settings:
  verifySsl: false
  timeoutMs: 5000
  caCert: certs/ca.pem
  useCustomCa: true
custom:
  keep: me
"#;

#[test]
fn parses_environments_in_written_order() {
    let envs = parse_environments(APP_YML);
    assert_eq!(envs.keys().collect::<Vec<_>>(), vec!["dev", "prod"], "环境顺序按书写序");
    let dev = env_vars(&envs, "dev");
    assert_eq!(dev.get("baseUrl").map(String::as_str), Some("http://localhost:8080"));
    assert_eq!(dev.get("retries").map(String::as_str), Some("3"), "非字符串值转成字符串");
    assert!(env_vars(&envs, "不存在").is_empty());
}

#[test]
fn parses_settings() {
    let s = parse_settings(APP_YML);
    assert!(!s.verify_ssl);
    assert!(s.use_custom_ca);
    assert_eq!(s.ca_cert, "certs/ca.pem");
    assert_eq!(s.timeout_ms, 5000);
}

/// 配置文件是手写的，一处写坏不该让请求功能整体瘫掉
#[test]
fn broken_config_falls_back_to_defaults() {
    for bad in ["这不是: [有效\n  yaml", "", "environment: 不是对象\n", "settings: 42\n"] {
        let s = parse_settings(bad);
        assert_eq!(s, WorkspaceSettings::default(), "坏配置应回落默认：{bad:?}");
        assert!(parse_environments(bad).is_empty());
    }
    // 超时写成负数 / 非数字 → 0（不限制）
    assert_eq!(parse_settings("settings:\n  timeoutMs: -5\n").timeout_ms, 0);
    assert_eq!(parse_settings("settings:\n  timeoutMs: 很久\n").timeout_ms, 0);
    // 只有显式 false 才关校验
    assert!(parse_settings("settings:\n  verifySsl: 随便\n").verify_ssl);
}

/// 写回时必须保留原文里我们不认识的顶层键 —— 否则一次可视化保存就把它们吃掉了
#[test]
fn dump_app_config_preserves_unknown_keys() {
    let envs = parse_environments(APP_YML);
    let st = parse_settings(APP_YML);
    let out = dump_application_config(APP_YML, &envs, Some(&st));
    assert!(out.contains("custom:\n  keep: me\n"), "未知顶层键应保留：\n{out}");
    // 再解析一轮，环境与设置不变
    assert_eq!(parse_settings(&out), st);
    assert_eq!(parse_environments(&out), envs);
}

/// settings 全为默认 → 整键删除，不给既有配置添噪
#[test]
fn dump_app_config_drops_all_default_settings() {
    let envs = parse_environments(APP_YML);
    let out = dump_application_config(APP_YML, &envs, Some(&WorkspaceSettings::default()));
    assert!(!out.contains("settings:"), "全默认时不该写 settings 键：\n{out}");
}

/// settings 里用户手写的其它键（未来字段）要留着
#[test]
fn dump_app_config_keeps_unknown_settings_keys() {
    let base = "settings:\n  verifySsl: false\n  myFutureFlag: true\n";
    let out = dump_application_config(base, &Environments::new(), Some(&parse_settings(base)));
    assert!(out.contains("myFutureFlag: true"), "未知 settings 键应保留：\n{out}");
    assert!(out.contains("verifySsl: false"), "{out}");
}

/// settings 传 None 时完全不动原文该键
#[test]
fn dump_app_config_leaves_settings_untouched_when_none() {
    let out = dump_application_config(APP_YML, &parse_environments(APP_YML), None);
    assert_eq!(parse_settings(&out), parse_settings(APP_YML));
}
