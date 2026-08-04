use super::*;
use crate::http::{ProxyConfig, RequestOptions};
use crate::testutil::{MockServer, Recorded, Reply};
use serde_json::json;

/// 一律走 `proxy: none`：开发机上常设 HTTPS_PROXY，不绕开就根本到不了本地 mock。
fn direct(env_vars: &[(&str, &str)]) -> RunOpts {
    let vars: BTreeMap<String, String> =
        env_vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let mut o = RunOpts::for_batch(EnvironmentInfo { name: "test".into(), vars });
    o.client = ClientConfig {
        proxy: Some(ProxyConfig { mode: "none".into(), url: None }),
        options: Some(RequestOptions { timeout_ms: Some(10_000), ..Default::default() }),
        ..Default::default()
    };
    o
}

// ── 拓扑序 ──────────────────────────────────────────

fn steps_with_deps(spec: &[(&str, &[&str])]) -> Vec<Step> {
    spec.iter()
        .map(|(id, deps)| Step {
            id: (*id).into(),
            protocol: "http".into(),
            ui: None,
            http: Default::default(),
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            outputs: vec![],
            assertions: vec![],
            docs: None,
        })
        .collect()
}

#[test]
fn topo_order_respects_dependencies() {
    let steps = steps_with_deps(&[("c", &["b"]), ("a", &[]), ("b", &["a"])]);
    let order: Vec<&str> = topo_order(&steps).iter().map(|&i| steps[i].id.as_str()).collect();
    assert_eq!(order, vec!["a", "b", "c"]);
}

/// 环是配置错误，但不该让运行卡死——按出现序兜底跑完
#[test]
fn topo_order_survives_cycles() {
    let steps = steps_with_deps(&[("a", &["b"]), ("b", &["a"])]);
    let order = topo_order(&steps);
    assert_eq!(order.len(), 2, "成环时仍要把每个 step 排进去一次");

    // 自环
    let steps = steps_with_deps(&[("a", &["a"])]);
    assert_eq!(topo_order(&steps).len(), 1);

    // 指向不存在的 step：忽略该依赖，不丢掉这一步
    let steps = steps_with_deps(&[("a", &["幽灵"])]);
    assert_eq!(topo_order(&steps).len(), 1);
}

/// `--step` 的地基：点名一个 step，上游要跟着进来，无关分支不进来
#[test]
fn with_dependencies_pulls_in_the_upstream_closure() {
    // login → order → pay，另有一条独立的 health
    let steps = steps_with_deps(&[
        ("login", &[]),
        ("order", &["login"]),
        ("pay", &["order"]),
        ("health", &[]),
    ]);
    let ids = |v: Vec<usize>| -> Vec<&str> { v.into_iter().map(|i| steps[i].id.as_str()).collect() };

    assert_eq!(
        ids(with_dependencies(&steps, &["pay".into()])),
        vec!["login", "order", "pay"],
        "隔着两层的上游也要补齐，且按拓扑序"
    );
    assert_eq!(ids(with_dependencies(&steps, &["health".into()])), vec!["health"], "独立分支不带别人进来");
    assert_eq!(
        ids(with_dependencies(&steps, &["health".into(), "order".into()])),
        vec!["login", "order", "health"],
        "多个点名取并集，顺序仍由拓扑序决定"
    );
    assert!(with_dependencies(&steps, &["幽灵".into()]).is_empty(), "认不出的 id 忽略");
    assert!(with_dependencies(&steps, &[]).is_empty());
}

/// 成环时不能死循环（环是配置错误，但 --step 不该因此挂住）
#[test]
fn with_dependencies_survives_cycles() {
    let steps = steps_with_deps(&[("a", &["b"]), ("b", &["a"]), ("c", &[])]);
    assert_eq!(with_dependencies(&steps, &["a".into()]).len(), 2, "环里的两个都收下，收一次");
}

// ── 跳过传播 ────────────────────────────────────────

/// 传播沿依赖闭包走：连累链上的每一个，且**根因始终指向最上面那个**。
#[test]
fn blocking_propagates_along_the_dependency_closure() {
    // a → b → c 一条链，d 独立
    let steps = steps_with_deps(&[("a", &[]), ("b", &["a"]), ("c", &["b"]), ("d", &[])]);
    let out = blocked_steps(&steps, &["a".to_string()]);
    assert_eq!(
        out,
        vec![
            BlockedStep { id: "b".into(), cause: "a".into() },
            BlockedStep { id: "c".into(), cause: "a".into() },
        ],
        "c 隔了一层也要被连累，且根因是 a 而不是 b"
    );
}

/// DAG 的价值所在：独立分支不该被另一条分支连累
#[test]
fn independent_branches_are_untouched() {
    // 左：a → b；右：x → y。两条互不相干
    let steps = steps_with_deps(&[("a", &[]), ("b", &["a"]), ("x", &[]), ("y", &["x"])]);
    let out = blocked_steps(&steps, &["a".to_string()]);
    assert_eq!(out.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), vec!["b"]);
}

/// 多个上游只要有一个挂了就跳过（等价于 GitHub Actions 的 `needs` 语义）
#[test]
fn any_failed_dependency_blocks() {
    let steps = steps_with_deps(&[("a", &[]), ("b", &[]), ("c", &["a", "b"])]);
    let out = blocked_steps(&steps, &["b".to_string()]);
    assert_eq!(out, vec![BlockedStep { id: "c".into(), cause: "b".into() }]);
    // 没有阻断源时谁都不跳
    assert!(blocked_steps(&steps, &[]).is_empty());
}

/// 一个 case：a 失败 → b、c 被连累；d 独立照跑。
/// 返回 (各 step 的 (id, 状态, skipReason)、服务端实际收到的路径)
async fn run_chain_with_switch(
    continue_on_assertion_failure: bool,
    a_path: &str,
) -> (Vec<(String, StepStatus, Option<String>)>, Vec<String>, CaseStatus) {
    let srv = MockServer::start(|r: &Recorded| match r.path.as_str() {
        "/bad" => Reply::json(r#"{"code":1}"#), // 200 但业务码不对 → 断言失败
        _ => Reply::json(r#"{"code":0}"#),
    })
    .await;
    // a 挂 → b → c；d 独立
    let text = format!(
        "apicase: v0.1\nsteps:\n\
         \x20 - id: a\n    request:\n      method: GET\n      url: {b}{a_path}\n\
         \x20   assertions:\n      - target: res.body.code\n        op: eq\n        value: '0'\n\
         \x20 - id: b\n    dependsOn:\n      - a\n    request:\n      method: GET\n      url: {b}/b\n\
         \x20 - id: c\n    dependsOn:\n      - b\n    request:\n      method: GET\n      url: {b}/c\n\
         \x20 - id: d\n    request:\n      method: GET\n      url: {b}/d\n",
        b = srv.base
    );
    let mut opts = direct(&[]);
    opts.continue_on_assertion_failure = continue_on_assertion_failure;
    let r = run_case(&text, "chain.yml", &opts, &Cancel::new()).await;
    let steps = r.steps.iter().map(|s| (s.id.clone(), s.status, s.skip_reason.clone())).collect();
    let mut paths: Vec<String> = srv.requests().iter().map(|q| q.path.clone()).collect();
    paths.sort();
    (steps, paths, r.status)
}

/// `run_case` 的流式传播与 `blocked_from_outcomes` 的批量传播**必须给出同一答案**。
///
/// 两者是同一条规则的两份代码：前者边跑边算（每跑完一步才知道状态），后者从结果集
/// 一次算完（前端驱动的调试运行用它）。没有这条测试，改其中一处就会让
/// 「界面里跑」和「批量跑」悄悄分道扬镳——正是执行内核下沉要消灭的那种漂移。
#[tokio::test]
async fn streaming_and_batch_propagation_agree() {
    for switch in [false, true] {
        let (steps, _, _) = run_chain_with_switch(switch, "/bad").await;
        let from_run: Vec<&str> = steps
            .iter()
            .filter(|(_, st, _)| *st == StepStatus::Skipped)
            .map(|(id, _, _)| id.as_str())
            .collect();

        // 拿同一批结果喂给批量传播。**只回报真正跑过的**——跳过的是它的产出，不是输入
        let specs = steps_with_deps(&[("a", &[]), ("b", &["a"]), ("c", &["b"]), ("d", &[])]);
        let outcomes: Vec<StepOutcome> = steps
            .iter()
            .filter(|(_, st, _)| *st != StepStatus::Skipped)
            .map(|(id, st, _)| StepOutcome { id: id.clone(), status: *st })
            .collect();
        let from_batch: Vec<String> =
            blocked_from_outcomes(&specs, &outcomes, switch).into_iter().map(|b| b.id).collect();

        assert_eq!(from_run, from_batch, "开关={switch} 时两条路径应给出同一批跳过");
    }
}

/// 默认行为：断言失败阻断下游，且**被跳过的节点仍进报告**
#[tokio::test]
async fn assertion_failure_blocks_downstream_by_default() {
    let (steps, paths, status) = run_chain_with_switch(false, "/bad").await;
    assert_eq!(
        steps.iter().map(|(id, st, _)| (id.as_str(), *st)).collect::<Vec<_>>(),
        vec![
            ("a", StepStatus::Failed),
            ("b", StepStatus::Skipped),
            ("c", StepStatus::Skipped),
            ("d", StepStatus::Passed),
        ],
        "b/c 被连累，d 是独立分支照跑"
    );
    // 根因指向 a，隔了一层的 c 也是
    assert_eq!(steps[1].2.as_deref(), Some("上游 a 失败"));
    assert_eq!(steps[2].2.as_deref(), Some("上游 a 失败"), "c 的根因是 a，不是 b");
    assert_eq!(paths, vec!["/bad", "/d"], "b、c 的请求根本不该发出去");
    // skipped 不稀释 case 状态——它该指向根因那一步
    assert_eq!(status, CaseStatus::Failed);
}

/// 开关打开：断言失败不再阻断，整条链跑完
#[tokio::test]
async fn continue_on_assertion_failure_lets_the_chain_run() {
    let (steps, paths, status) = run_chain_with_switch(true, "/bad").await;
    assert!(
        steps.iter().all(|(_, st, _)| *st != StepStatus::Skipped),
        "开关打开后不该有跳过：{steps:?}"
    );
    assert_eq!(paths, vec!["/b", "/bad", "/c", "/d"], "四个请求都发出去了");
    assert_eq!(status, CaseStatus::Failed, "断言仍然是没过的，只是不阻断");
}

/// **error 恒阻断**——开关只管断言失败，管不着"请求根本没发出去"
#[tokio::test]
async fn error_blocks_downstream_even_with_the_switch_on() {
    let srv = MockServer::start(|_| Reply::json(r#"{"code":0}"#)).await;
    // a 指向一个连不上的端口 → error
    let text = format!(
        "apicase: v0.1\nsteps:\n\
         \x20 - id: a\n    request:\n      method: GET\n      url: http://127.0.0.1:1/a\n\
         \x20 - id: b\n    dependsOn:\n      - a\n    request:\n      method: GET\n      url: {b}/b\n",
        b = srv.base
    );
    let mut opts = direct(&[]);
    opts.continue_on_assertion_failure = true;
    let r = run_case(&text, "e.yml", &opts, &Cancel::new()).await;
    assert_eq!(r.steps[0].status, StepStatus::Error);
    assert_eq!(r.steps[1].status, StepStatus::Skipped, "error 不受开关影响");
    assert_eq!(r.steps[1].skip_reason.as_deref(), Some("上游 a 失败"));
    assert!(srv.requests().is_empty(), "下游请求不该打到被测服务上");
    assert_eq!(r.status, CaseStatus::Error);
}

// ── 端到端：单个 case ───────────────────────────────

const LOGIN_CASE: &str = r#"
apicase: v0.1
name: 登录后取用户
steps:
  - id: login
    protocol: http
    request:
      method: POST
      url: ${{base}}/login
      body:
        type: json
        json:
          user: alice
    outputs:
      token: res.body.data.token
    assertions:
      - target: res.status
        op: eq
        value: 200
      - target: res.body.data.token
        op: exists
  - id: profile
    protocol: http
    dependsOn:
      - login
    request:
      method: GET
      url: ${{base}}/me
      auth:
        type: bearer
        bearer:
          token: ${{steps.login.outputs.token}}
    assertions:
      - target: res.body.name
        op: eq
        value: alice
"#;

#[tokio::test]
async fn runs_a_multi_step_case_end_to_end() {
    let srv = MockServer::start(|req| match req.path.as_str() {
        "/login" => Reply::json(r#"{"data":{"token":"tok-abcdef-123"}}"#),
        "/me" => {
            // 只有带对 token 才认
            if req.header("authorization") == Some("Bearer tok-abcdef-123") {
                Reply::json(r#"{"name":"alice"}"#)
            } else {
                Reply::status(401).with_body("no token")
            }
        }
        _ => Reply::status(404),
    })
    .await;

    let opts = direct(&[("base", &srv.base)]);
    let r = run_case(LOGIN_CASE, "a/login.yml", &opts, &Cancel::new()).await;

    assert_eq!(r.status, CaseStatus::Passed, "{r:#?}");
    assert_eq!(r.name, "登录后取用户");
    assert_eq!(r.file, "a/login.yml");
    assert_eq!(r.steps.len(), 2);
    assert!(r.steps.iter().all(|s| s.status == StepStatus::Passed));

    // 上游 outputs 确实流到了下游的请求头里
    let me = srv.requests().into_iter().find(|q| q.path == "/me").expect("应发过 /me");
    assert_eq!(me.header("authorization"), Some("Bearer tok-abcdef-123"));

    // 请求体是组装出来的 JSON
    let login = srv.requests().into_iter().find(|q| q.path == "/login").expect("应发过 /login");
    assert_eq!(login.method, "POST");
    assert!(login.body.contains("alice"), "{}", login.body);
    assert_eq!(login.header("content-type"), Some("application/json; charset=utf-8"));
}

/// 断言没过是 failed（被测服务的问题），请求发不出去是 error（环境/用例的问题）。
/// 混成一个状态就丢掉了最有用的那点信息。
#[tokio::test]
async fn failed_and_error_are_distinguished() {
    let srv = MockServer::start(|_| Reply::json(r#"{"code":1}"#)).await;
    let opts = direct(&[("base", &srv.base)]);

    let failing = format!(
        "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: {}/x\n    assertions:\n      - target: res.body.code\n        op: eq\n        value: '0'\n",
        srv.base
    );
    let r = run_case(&failing, "f.yml", &opts, &Cancel::new()).await;
    assert_eq!(r.status, CaseStatus::Failed);
    assert_eq!(r.steps[0].status, StepStatus::Failed);
    assert!(r.steps[0].response.is_some(), "failed 时响应是有的");
    assert!(r.steps[0].error.is_none());

    // 连不上的端口 → error
    let broken = "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: http://127.0.0.1:1/x\n";
    let r = run_case(broken, "e.yml", &opts, &Cancel::new()).await;
    assert_eq!(r.status, CaseStatus::Error);
    assert_eq!(r.steps[0].status, StepStatus::Error);
    assert!(r.steps[0].error.is_some());
    // 请求发不出去也要记下**将要发送**的报文，否则连打到哪个 URL 都看不出来
    assert_eq!(r.steps[0].request.as_ref().unwrap().url, "http://127.0.0.1:1/x");
}

#[tokio::test]
async fn invalid_cases_are_skipped_with_a_reason() {
    let opts = direct(&[]);
    let c = Cancel::new();
    for (text, hint) in [
        ("这不是: [有效\n  yaml", "YAML"),
        ("name: 没有 steps\n", "steps"),
        ("apicase: v0.1\nsteps: []\n", "请求"),
    ] {
        let r = run_case(text, "x.yml", &opts, &c).await;
        assert_eq!(r.status, CaseStatus::Skipped, "{text}");
        let reason = r.skip_reason.expect("必须给出原因——静默跳过会让人以为全跑过了");
        assert!(reason.contains(hint), "原因应提到 {hint}，实际：{reason}");
    }
}

/// 变量隔离：case 之间不共享 outputs，否则一开并发就是竞态
#[tokio::test]
async fn cases_do_not_share_outputs() {
    let srv = MockServer::start(|_| Reply::json(r#"{"v":"从上一个 case 泄漏"}"#)).await;
    let opts = direct(&[("base", &srv.base)]);

    let producer = format!(
        "apicase: v0.1\nsteps:\n  - id: p\n    request:\n      method: GET\n      url: {}/a\n    outputs:\n      leaked: res.body.v\n",
        srv.base
    );
    let consumer = format!(
        "apicase: v0.1\nsteps:\n  - id: c\n    request:\n      method: GET\n      url: {}/b?got=${{{{steps.p.outputs.leaked}}}}\n",
        srv.base
    );
    let c = Cancel::new();
    run_case(&producer, "p.yml", &opts, &c).await;
    run_case(&consumer, "c.yml", &opts, &c).await;

    let last = srv.requests().pop().expect("应有请求");
    assert!(last.path.contains("%7B%7Bsteps.p.outputs.leaked%7D%7D") || last.path.contains("${{steps.p.outputs.leaked}}"),
        "另一个 case 的 outputs 不该可见，未解析的变量应原样发出：{}", last.path);
}

/// case 级 vars 覆盖 environment
#[tokio::test]
async fn case_vars_override_environment() {
    let srv = MockServer::start(|_| Reply::json("{}")).await;
    let opts = direct(&[("who", "环境")]);
    let text = format!(
        "apicase: v0.1\nvars:\n  who: 用例\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: {}/x?w=${{{{who}}}}\n",
        srv.base
    );
    run_case(&text, "v.yml", &opts, &Cancel::new()).await;
    let q = srv.requests().pop().unwrap();
    assert!(q.path.contains("w=%E7%94%A8%E4%BE%8B") || q.path.contains("w=用例"), "{}", q.path);
}

// ── 凭据透传 ────────────────────────────────────────

/// 登录拿 token → 下游带着它发请求，**报告里记的是真实值**。
///
/// 脱敏已整体移除：报告是给自己和团队看的排查材料，掩码掉的恰恰是排查时要核对的东西。
/// 代价是报告含明文凭据，**转发或提交前需自行判断**。
#[tokio::test]
async fn credentials_flow_through_and_are_recorded_verbatim() {
    let srv = MockServer::start(|req| match req.path.as_str() {
        "/login" => Reply::json(r#"{"data":{"token":"S3CR3T-TOKEN-VALUE"}}"#),
        // 回显请求头：凭据被服务端原样吐回来，是最常见的泄漏路径
        _ => Reply::json(format!(
            r#"{{"echoedAuth":"{}"}}"#,
            req.header("authorization").unwrap_or("")
        )),
    })
    .await;

    let text = format!(
        r#"apicase: v0.1
steps:
  - id: login
    request:
      method: POST
      url: {base}/login
    outputs:
      token: res.body.data.token
  - id: use
    dependsOn:
      - login
    request:
      method: GET
      url: {base}/echo
      auth:
        type: bearer
        bearer:
          token: ${{{{steps.login.outputs.token}}}}
"#,
        base = srv.base
    );

    let r = run_case(&text, "s.yml", &direct(&[]), &Cancel::new()).await;
    assert_eq!(r.status, CaseStatus::Passed, "{r:#?}");

    // 下游真的带上了原值
    let echo = srv.requests().into_iter().find(|q| q.path == "/echo").unwrap();
    assert_eq!(echo.header("authorization"), Some("Bearer S3CR3T-TOKEN-VALUE"));

    // 报告的四个落点都是原值——凭据在哪一步变成什么，逐处可核对
    assert_eq!(r.steps[0].outputs.get("token"), Some(&json!("S3CR3T-TOKEN-VALUE")), "outputs 存原值");
    let req_auth = r.steps[1].request.as_ref().unwrap().headers.iter().find(|h| h.key == "Authorization").unwrap();
    assert_eq!(req_auth.value, "Bearer S3CR3T-TOKEN-VALUE", "请求头存原值");
    let body = r.steps[1].response.as_ref().unwrap().body.preview.as_ref().unwrap();
    assert!(body.contains("S3CR3T-TOKEN-VALUE"), "响应体原样：{body}");
}

/// 调试运行**不截断**——响应区要看的就是完整内容。
/// 截断只在写进报告时才有意义（单文件 HTML 会把报文体全内联）。
#[tokio::test]
async fn debug_mode_does_not_clip() {
    let big = "x".repeat(200_000);
    let srv = MockServer::start(move |_| Reply::json(format!(r#"{{"pad":"{big}"}}"#))).await;
    let mut opts = RunOpts::for_debug(EnvironmentInfo { name: "d".into(), vars: BTreeMap::new() });
    opts.client = direct(&[]).client;
    let text = format!("apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: {}/x\n", srv.base);
    let r = run_case(&text, "d.yml", &opts, &Cancel::new()).await;
    let b = &r.steps[0].response.as_ref().unwrap().body;
    assert!(!b.truncated, "调试运行不截断");
    assert!(b.bytes > 200_000);
}

/// 大响应体按字节截断，但 `bytes` 记原始大小
#[tokio::test]
async fn large_bodies_are_clipped_in_reports() {
    let big = "x".repeat(200_000);
    let srv = MockServer::start(move |_| Reply::json(format!(r#"{{"pad":"{big}"}}"#))).await;
    let opts = direct(&[]);
    let text = format!("apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: {}/x\n", srv.base);
    let r = run_case(&text, "b.yml", &opts, &Cancel::new()).await;
    let b = &r.steps[0].response.as_ref().unwrap().body;
    assert!(b.truncated);
    assert_eq!(b.preview.as_ref().unwrap().len(), DEFAULT_MAX_BODY_BYTES);
    assert!(b.bytes > 200_000, "bytes 记的是原始大小：{}", b.bytes);
}

// ── 批量 ────────────────────────────────────────────

struct Fixture {
    _dir: std::path::PathBuf,
    targets: Vec<BatchTarget>,
}

/// 在临时目录里写出若干 case 文件。名字带上测试自己的标识，避免并行时互相覆盖。
fn fixture(tag: &str, files: &[(&str, String)]) -> Fixture {
    let dir = std::env::temp_dir().join(format!("apicase-runner-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建测试目录");
    let targets = files
        .iter()
        .map(|(name, text)| {
            let p = dir.join(name);
            std::fs::write(&p, text).expect("写用例");
            BatchTarget { file: (*name).into(), path: p.to_string_lossy().into_owned() }
        })
        .collect();
    Fixture { _dir: dir, targets }
}

fn meta() -> BatchMeta {
    BatchMeta {
        workspace: WorkspaceInfo { name: "w".into(), root: "/w".into() },
        tool_version: "0.1.0".into(),
        options: RunOptions {
            targets: vec![".".into()],
            recursive: true,
            environment: "test".into(),
            concurrency: 1,
            stop_on_failure: false,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            continue_on_assertion_failure: false,
        },
    }
}

fn case_hitting(base: &str, path: &str, expect_code: &str) -> String {
    format!(
        "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: {base}{path}\n    assertions:\n      - target: res.body.code\n        op: eq\n        value: '{expect_code}'\n"
    )
}

#[tokio::test]
async fn batch_reports_progress_and_summary() {
    let srv = MockServer::start(|_| Reply::json(r#"{"code":0}"#)).await;
    let f = fixture(
        "progress",
        &[
            ("1.yml", case_hitting(&srv.base, "/a", "0")),
            ("2.yml", case_hitting(&srv.base, "/b", "999")), // 断言不过
            ("3.yml", "不是 case".into()),
        ],
    );

    let seen = Arc::new(std::sync::Mutex::new(Vec::<(usize, RunStatus)>::new()));
    let s2 = seen.clone();
    let report = run_batch(
        f.targets.clone(),
        meta(),
        direct(&[]),
        Some(Arc::new(move |r: &RunReport| s2.lock().unwrap().push((r.cases.len(), r.status)))),
        Cancel::new(),
    )
    .await;

    assert_eq!(report.status, RunStatus::Done);
    assert_eq!(report.summary.total, 3);
    assert_eq!((report.summary.passed, report.summary.failed, report.summary.skipped), (1, 1, 1));
    assert!(report.finished_at.is_some());

    // 进度确实是逐条冒出来的：开跑一次 + 每个 case 一次 + 收尾一次
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.first(), Some(&(0, RunStatus::Running)), "开跑时先发一次空快照");
    assert_eq!(seen.last(), Some(&(3, RunStatus::Done)));
    assert_eq!(seen.len(), 5, "1 次开跑 + 3 次完成 + 1 次收尾：{seen:?}");
    // 中间态的条数必须单调递增，否则前端的进度会来回跳
    let counts: Vec<usize> = seen.iter().map(|(n, _)| *n).collect();
    assert!(counts.windows(2).all(|w| w[0] <= w[1]), "{counts:?}");
}

/// 读不到的文件记为 skipped 并说明原因，不静默丢弃
#[tokio::test]
async fn unreadable_files_are_skipped_not_dropped() {
    let report = run_batch(
        vec![BatchTarget { file: "gone.yml".into(), path: "/绝无此路径/gone.yml".into() }],
        meta(),
        direct(&[]),
        None,
        Cancel::new(),
    )
    .await;
    assert_eq!(report.summary.skipped, 1);
    assert!(report.cases[0].skip_reason.as_ref().unwrap().contains("读取失败"));
}

#[tokio::test]
async fn cancellation_stops_at_case_boundary() {
    let srv = MockServer::start(|_| Reply::json(r#"{"code":0}"#)).await;
    let files: Vec<(&str, String)> = vec![
        ("1.yml", case_hitting(&srv.base, "/a", "0")),
        ("2.yml", case_hitting(&srv.base, "/b", "0")),
        ("3.yml", case_hitting(&srv.base, "/c", "0")),
    ];
    let f = fixture("cancel", &files);

    let cancel = Cancel::new();
    let c2 = cancel.clone();
    let report = run_batch(
        f.targets.clone(),
        meta(),
        direct(&[]),
        // 跑完第一个就取消
        Some(Arc::new(move |r: &RunReport| {
            if !r.cases.is_empty() {
                c2.cancel();
            }
        })),
        cancel,
    )
    .await;

    assert_eq!(report.status, RunStatus::Cancelled);
    assert_eq!(report.cases.len(), 1, "取消后不再开新的 case");
    // 已经跑完的那个结果是完整的，不是半截
    assert_eq!(report.cases[0].status, CaseStatus::Passed);
}

#[tokio::test]
async fn stop_on_failure_halts_the_batch() {
    let srv = MockServer::start(|_| Reply::json(r#"{"code":1}"#)).await;
    let f = fixture(
        "stopfail",
        &[
            ("1.yml", case_hitting(&srv.base, "/a", "1")), // 通过
            ("2.yml", case_hitting(&srv.base, "/b", "0")), // 失败
            ("3.yml", case_hitting(&srv.base, "/c", "1")), // 不该跑到
        ],
    );
    let mut opts = direct(&[]);
    opts.stop_on_failure = true;
    let report = run_batch(f.targets.clone(), meta(), opts, None, Cancel::new()).await;
    assert_eq!(report.cases.len(), 2);
    assert_eq!(report.status, RunStatus::Done, "主动停止不算「取消」");
}

/// step 级的 stop_on_failure：一步没过就不再跑后续步骤
#[tokio::test]
async fn stop_on_failure_also_halts_steps_within_a_case() {
    let srv = MockServer::start(|_| Reply::json(r#"{"code":1}"#)).await;
    let text = format!(
        "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: {b}/a\n    assertions:\n      - target: res.body.code\n        op: eq\n        value: '0'\n  - id: b\n    dependsOn:\n      - a\n    request:\n      method: GET\n      url: {b}/b\n",
        b = srv.base
    );
    let mut opts = direct(&[]);
    opts.stop_on_failure = true;
    let r = run_case(&text, "x.yml", &opts, &Cancel::new()).await;
    assert_eq!(r.steps.len(), 1, "第一步没过就不该跑第二步");
    assert!(!srv.requests().iter().any(|q| q.path == "/b"));
}

// ── 并发 ────────────────────────────────────────────

#[tokio::test]
async fn concurrency_runs_cases_in_parallel() {
    // 每个请求睡 120ms：串行 6 个要 ~720ms，4 并发应显著更快
    let srv = MockServer::start(|_| Reply::json(r#"{"code":0}"#).after(120)).await;
    let files: Vec<(&str, String)> = ["1.yml", "2.yml", "3.yml", "4.yml", "5.yml", "6.yml"]
        .iter()
        .map(|n| (*n, case_hitting(&srv.base, "/x", "0")))
        .collect();
    let f = fixture("concurrent", &files);

    let mut opts = direct(&[]);
    opts.concurrency = 4;
    let t0 = std::time::Instant::now();
    let report = run_batch(f.targets.clone(), meta(), opts, None, Cancel::new()).await;
    let elapsed = t0.elapsed();

    assert_eq!(report.summary.total, 6);
    assert_eq!(report.summary.passed, 6, "并发不该改变结果");
    assert!(elapsed.as_millis() < 600, "4 并发跑 6 个 120ms 的用例应远快于串行，实际 {elapsed:?}");
}

/// 并发下取消同样在 case 边界生效，且不会丢结果
#[tokio::test]
async fn concurrent_cancellation_is_clean() {
    let srv = MockServer::start(|_| Reply::json(r#"{"code":0}"#).after(60)).await;
    let files: Vec<(&str, String)> =
        (1..=10).map(|i| (Box::leak(format!("{i}.yml").into_boxed_str()) as &str, case_hitting(&srv.base, "/x", "0"))).collect();
    let f = fixture("concurrent-cancel", &files);

    let cancel = Cancel::new();
    let c2 = cancel.clone();
    let mut opts = direct(&[]);
    opts.concurrency = 3;
    let report = run_batch(
        f.targets.clone(),
        meta(),
        opts,
        Some(Arc::new(move |r: &RunReport| {
            if r.cases.len() >= 3 {
                c2.cancel();
            }
        })),
        cancel,
    )
    .await;

    assert_eq!(report.status, RunStatus::Cancelled);
    assert!(report.cases.len() >= 3 && report.cases.len() < 10, "实际 {}", report.cases.len());
    // 汇总必须与实际收到的 case 一致（不能因为并发就对不上账）
    assert_eq!(report.summary.total as usize, report.cases.len());
}

/// 连接复用：同一个客户端连续打同一个服务，只该开一条 TCP 连接。
/// 这是把执行下沉到 Rust 之后最直接的性能收益，值得钉住。
#[tokio::test]
async fn connections_are_reused_across_cases() {
    // 不清全局客户端池：mock server 每次都是新端口，池里既有的连接不可能通到它，
    // 首个请求必然新建连接。反倒是清了会打断并行跑的别的测试的连接复用。
    let srv = MockServer::start(|_| Reply::json(r#"{"code":0}"#)).await;
    let files: Vec<(&str, String)> = ["1.yml", "2.yml", "3.yml", "4.yml", "5.yml"]
        .iter()
        .map(|n| (*n, case_hitting(&srv.base, "/x", "0")))
        .collect();
    let f = fixture("keepalive", &files);

    let report = run_batch(f.targets.clone(), meta(), direct(&[]), None, Cancel::new()).await;
    assert_eq!(report.summary.passed, 5);
    assert_eq!(srv.request_count(), 5);
    assert_eq!(srv.connection_count(), 1, "5 个请求应复用同一条连接，实际开了 {} 条", srv.connection_count());
}

// ── 认证（端到端）────────────────────────────────────

/// OAuth 2.0：换一次 token，后续复用缓存；报告里记的是真实 token
///
/// 刻意**不调用 `clear_token_cache()`**：那清的是进程级缓存，会把并行跑的
/// 别的测试正在用的条目一起清掉（表现为"莫名其妙多换了一次 token"）。
/// 每个测试的 mock server 端口不同，缓存键因而天然唯一，无需清理。
#[tokio::test]
async fn oauth2_exchanges_token_once_and_reuses_it() {
    let srv = MockServer::start(|req| match req.path.as_str() {
        "/token" => Reply::json(r#"{"access_token":"OA2-TOKEN-XYZ","token_type":"bearer","expires_in":3600}"#),
        _ => Reply::json(format!(r#"{{"seen":"{}"}}"#, req.header("authorization").unwrap_or(""))),
    })
    .await;

    let text = format!(
        r#"apicase: v0.1
steps:
  - id: a
    request:
      method: GET
      url: {base}/x
      auth:
        type: oauth2
        oauth2:
          tokenUrl: {base}/token
          clientId: cid
          clientSecret: csec
"#,
        base = srv.base
    );
    let f = fixture("oauth2", &[("1.yml", text.clone()), ("2.yml", text)]);
    let report = run_batch(f.targets.clone(), meta(), direct(&[]), None, Cancel::new()).await;
    assert_eq!(report.summary.passed, 2, "{report:#?}");

    let token_calls = srv.requests().iter().filter(|q| q.path == "/token").count();
    assert_eq!(token_calls, 1, "两个 case 只该换一次 token，实际 {token_calls} 次");

    let api = srv.requests().into_iter().filter(|q| q.path == "/x").collect::<Vec<_>>();
    assert_eq!(api.len(), 2);
    assert!(api.iter().all(|q| q.header("authorization") == Some("Bearer OA2-TOKEN-XYZ")));
}

/// 并发下同一套 OAuth 2.0 配置也只换一次 token（in-flight 去重）。
/// 这是 case 之间敢开并发的前提：没有它，8 个并发就是 8 次 token 交换，
/// 既慢又可能触发授权服务器限流。
#[tokio::test]
async fn oauth2_dedupes_under_concurrency() {
    let srv = MockServer::start(|req| match req.path.as_str() {
        // token 端点故意慢：没有去重的话，并发请求会全部挤进来
        "/token" => Reply::json(r#"{"access_token":"T","token_type":"Bearer","expires_in":3600}"#).after(80),
        _ => Reply::json("{}"),
    })
    .await;

    let text = format!(
        "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: {base}/x\n      auth:\n        type: oauth2\n        oauth2:\n          tokenUrl: {base}/token\n          clientId: cid\n          clientSecret: csec\n",
        base = srv.base
    );
    let files: Vec<(&str, String)> =
        (1..=6).map(|i| (Box::leak(format!("{i}.yml").into_boxed_str()) as &str, text.clone())).collect();
    let f = fixture("oauth2-concurrent", &files);

    let mut opts = direct(&[]);
    opts.concurrency = 6;
    let report = run_batch(f.targets.clone(), meta(), opts, None, Cancel::new()).await;
    assert_eq!(report.summary.total, 6);
    let token_calls = srv.requests().iter().filter(|q| q.path == "/token").count();
    assert_eq!(token_calls, 1, "6 个并发只该换一次 token，实际 {token_calls} 次");
}

/// Digest：首发吃 401，就着 challenge 算摘要重发
#[tokio::test]
async fn digest_retries_with_the_challenge() {
    let srv = MockServer::start(|req| match req.header("authorization") {
        Some(a) if a.starts_with("Digest ") && a.contains("username=\"u\"") => Reply::json(r#"{"ok":true}"#),
        _ => Reply::status(401)
            .with_header("WWW-Authenticate", r#"Digest realm="r", nonce="n", qop="auth""#)
            .with_body("unauthorized"),
    })
    .await;

    let text = format!(
        "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: {}/secure\n      auth:\n        type: digest\n        digest:\n          username: u\n          password: p\n    assertions:\n      - target: res.status\n        op: eq\n        value: '200'\n",
        srv.base
    );
    let r = run_case(&text, "d.yml", &direct(&[]), &Cancel::new()).await;
    assert_eq!(r.status, CaseStatus::Passed, "{r:#?}");
    assert_eq!(srv.request_count(), 2, "应当是「首发 401 + 带摘要重发」两次");
    // 报告里记的是**实际发出去**的那一份（带摘要头）
    let auth = r.steps[0].request.as_ref().unwrap().headers.iter().find(|h| h.key == "Authorization");
    let auth = auth.expect("报告应记录实际发出的认证头").value.as_str();
    assert!(auth.starts_with("Digest "), "记的应是摘要头本身：{auth}");
    assert!(auth.contains(r#"username="u""#), "摘要头内容原样可核对：{auth}");
}
