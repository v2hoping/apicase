//! 退出码的端到端测试：真的把二进制跑起来，看它返回什么。
//!
//! # 为什么值得单独测
//!
//! 退出码是 CLI 与 CI 之间**唯一的契约**，而它是最容易在重构中悄悄改掉的东西——
//! 单测能覆盖「算出来的码对不对」，覆盖不了「这个码有没有真的被返回」（中间隔着
//! `ExitCode::from`、`?` 的早退、panic）。这里从进程外面看。
//!
//! `1` 与 `3` 的区分是 apicase 特有的：断言没过是**被测服务**的问题，
//! 请求发不出去是**环境或用例自身**的问题。CI 里这两者的处理方式完全不同。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_apicase");

/// 起一个只会说一句话的 HTTP 服务，返回它的端口。
///
/// 不用 mock 库也不复用 core 的测试服务（那个是 `#[cfg(test)]` 私有的）：
/// 这里只需要「连得上、回 200、body 是一段 JSON」，手写比引依赖省事。
fn tiny_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("监听端口");
    let port = listener.local_addr().expect("取端口").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut s = stream;
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf); // 请求内容不看，回什么都一样
            let body = br#"{"status":"ok"}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body);
            let _ = s.flush();
        }
    });
    port
}

struct Ws(PathBuf);

impl Ws {
    /// 每个测试一个独立目录：它们并行跑，共用目录会互相踩报告与 cookie jar。
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("apicase-cli-it-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建工作空间");
        Self(dir)
    }

    fn file(&self, rel: &str, content: &str) -> &Self {
        let p = self.0.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).expect("建目录");
        }
        std::fs::write(p, content).expect("写文件");
        self
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            // 命令行给的相对路径按**进程工作目录**解析（CLI 的通例），
            // 所以要像真实用法那样先站进工作空间里
            .current_dir(&self.0)
            .arg("-w")
            .arg(&self.0)
            // 开发机常设 HTTPS_PROXY，不绕开就连不到本地服务
            .env("APICASE_PROXY", "none")
            .env("NO_COLOR", "1")
            .output()
            .expect("跑得起来")
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// 0 = 全部通过
#[test]
fn all_passing_exits_zero() {
    let port = tiny_server();
    let ws = Ws::new("pass");
    ws.file("application.yml", "environment:\n  dev: {}\n").file(
        "ok.yml",
        &format!(
            "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: http://127.0.0.1:{port}/x\n\
             \n    assertions:\n      - {{ target: res.status, op: eq, value: 200 }}\n"
        ),
    );
    let out = ws.run(&["run", "--no-report"]);
    assert_eq!(code(&out), 0, "stdout:\n{}", stdout(&out));
}

/// 1 = 断言没过（请求发出去了，是**被测服务**的问题）
#[test]
fn assertion_failure_exits_one() {
    let port = tiny_server();
    let ws = Ws::new("failed");
    ws.file("application.yml", "environment:\n  dev: {}\n").file(
        "bad.yml",
        &format!(
            "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: http://127.0.0.1:{port}/x\n\
             \n    assertions:\n      - {{ target: res.status, op: eq, value: 500 }}\n"
        ),
    );
    let out = ws.run(&["run", "--no-report"]);
    assert_eq!(code(&out), 1, "断言失败是 1，不是 3：\n{}", stdout(&out));
}

/// 3 = 请求根本没发出去（**环境或用例自身**的问题）
#[test]
fn transport_error_exits_three() {
    let ws = Ws::new("error");
    ws.file("application.yml", "environment:\n  dev: {}\n").file(
        "dead.yml",
        // 1 端口上不会有服务
        "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: http://127.0.0.1:1/nope\n",
    );
    let out = ws.run(&["run", "--no-report"]);
    assert_eq!(code(&out), 3, "连不上是 3，不是 1：\n{}", stdout(&out));
}

/// 读不成用例（语法错）也是 3——它没跑成，不是「跑了但没过」
#[test]
fn unparsable_case_exits_three() {
    let ws = Ws::new("broken");
    ws.file("application.yml", "environment:\n  dev: {}\n").file("broken.yml", "这不是: [有效\n  yaml\n");
    let out = ws.run(&["run", "--no-report"]);
    assert_eq!(code(&out), 3, "{}", stdout(&out));
}

/// 2 = 用法 / 配置错误。这一档要**在跑任何请求之前**就返回
#[test]
fn usage_errors_exit_two() {
    let ws = Ws::new("usage");
    ws.file("application.yml", "environment:\n  dev: {}\n");

    let cases: [(&[&str], &str); 3] = [
        (&["run", "不存在的目录"], "目标不存在"),
        (&["run", "--var", "没有等号"], "--var 格式"),
        (&["run"], "工作空间里没有用例"),
    ];
    for (args, why) in cases {
        let out = ws.run(args);
        assert_eq!(code(&out), 2, "{why} 应是 2：\n{}", String::from_utf8_lossy(&out.stderr));
        assert!(!out.stderr.is_empty(), "{why} 要在 stderr 上说清原因");
    }
}

/// 找不到工作空间也是 2，且要提示怎么办
#[test]
fn missing_workspace_exits_two_with_a_hint() {
    let dir = std::env::temp_dir().join("apicase-cli-it-nows");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let out = Command::new(BIN).arg("ls").current_dir(&dir).env("NO_COLOR", "1").output().expect("跑得起来");
    // 临时目录的上级可能真有 application.yml（本机环境），那样这条不适用
    if code(&out) == 2 {
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("apicase init"), "要告诉用户下一步做什么：{err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `check` 只有 error 才算没过——warning 不该把 CI 卡住
#[test]
fn check_exits_on_errors_but_not_on_warnings() {
    let ws = Ws::new("check");
    ws.file("application.yml", "environment:\n  dev: {}\n")
        // 只有 warning：断言目标认不出，但跑得动
        .file(
            "warn.yml",
            "apicase: v0.1\nsteps:\n  - id: a\n    request: { url: http://x }\n\
             \n    assertions:\n      - { target: status, op: eq, value: 200 }\n",
        );
    assert_eq!(code(&ws.run(&["check"])), 0, "只有警告时不该拦住 CI");

    // 加一个 error：依赖指向不存在的请求
    ws.file(
        "err.yml",
        "apicase: v0.1\nsteps:\n  - id: a\n    dependsOn: [幽灵]\n    request: { url: http://x }\n",
    );
    assert_eq!(code(&ws.run(&["check"])), 3, "有错误就该拦住");
}

/// `--json` 的输出必须是**干净的一份 JSON**：进度、提示、日志全在 stderr。
/// 脏了它，`apicase run --json | jq` 就直接崩。
#[test]
fn json_output_is_clean_on_stdout() {
    let port = tiny_server();
    let ws = Ws::new("json");
    ws.file("application.yml", "environment:\n  dev: {}\n").file(
        "ok.yml",
        &format!(
            "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: http://127.0.0.1:{port}/x\n"
        ),
    );
    let out = ws.run(&["run", "--json", "--no-report"]);
    let text = stdout(&out);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("stdout 不是 JSON（{e}）：\n{text}"));
    assert_eq!(v["schemaVersion"], 2, "报告 schema 是与界面共用的那一份");
    assert_eq!(v["summary"]["total"], 1);
}

/// 报告落盘的文件名要带上跑的是什么——一堆只有时间戳的文件分不出彼此
#[test]
fn report_lands_in_the_workspace_with_a_meaningful_name() {
    let port = tiny_server();
    let ws = Ws::new("report");
    ws.file("application.yml", "environment:\n  dev: {}\n").file(
        "冒烟.yml",
        &format!(
            "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: http://127.0.0.1:{port}/x\n"
        ),
    );
    let out = ws.run(&["run", "冒烟.yml"]);
    assert_eq!(code(&out), 0, "{}", stdout(&out));

    let dir = ws.path().join(".apicase/reports");
    let names: Vec<String> = std::fs::read_dir(&dir)
        .expect("报告目录应存在")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with("-冒烟.html"), "文件名要带目标名：{}", names[0]);
    assert!(names[0].len() > "-冒烟.html".len() + 10, "前缀是时间戳：{}", names[0]);

    // 报告与 cookie jar 都含明文凭据，跑一次就该把 .apicase/ 挡在版本库外
    let gi = std::fs::read_to_string(ws.path().join(".gitignore")).expect("应写了 .gitignore");
    assert!(gi.contains(".apicase/"), "{gi}");
}
