//! `apicase` 命令行入口。
//!
//! 这一层只做四件事：解析参数、找工作空间、调 `ops`、把结果渲染出去。
//! **没有业务逻辑**——业务在 `ops`（与 MCP 共用），执行语义在 `apicase-core`（与桌面端共用）。
//!
//! # 输出去哪
//!
//! - `--format text`（给人看）：全部走 **stdout**，它本身就是一份完整的报告。
//! - `--format json`（给机器看）：JSON 走 **stdout**，进度与提示走 **stderr**，
//!   这样 `apicase run --json | jq` 拿到的是干净的一份 JSON，而进度照样看得见。
//! - 错误恒走 stderr。

mod cli;
mod docs;
mod mcp;
mod ops;
mod render;
mod style;

use apicase_core::report::{RunReport, StepStatus};
use apicase_core::runner::Cancel;
use apicase_core::workspace::Workspace;
use clap::CommandFactory;
use cli::{Cli, Command, Detail, Format, GlobalOpts};
use ops::{CheckReport, OnEvent, ReportSink, RunRequest};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use style::Style;

/// 退出码。
///
/// `2` 留给用法错误是 GNU 工具的强约定（`grep`：0 匹配 / 1 不匹配 / 2 出错；`diff` 同）。
/// `1` 与 `3` 的区分则是 apicase 特有的：**`failed ≠ error` 是执行内核的核心理念**——
/// 断言没过是被测服务的问题，请求发不出去是环境或用例自身的问题，两者的排查方向完全不同。
/// 退出码若不体现它，等于在 CI 那一层把最有用的信息丢掉。
mod exit {
    /// 全部通过
    pub const OK: u8 = 0;
    /// 有断言失败——被测服务的问题
    pub const FAILED: u8 = 1;
    /// 用法 / 配置错误——工具层面
    pub const USAGE: u8 = 2;
    /// 有执行错误或用例没跑成——环境或用例自身的问题
    pub const ERROR: u8 = 3;
    /// 被 Ctrl-C 中断（128 + SIGINT，POSIX 惯例）
    pub const INTERRUPTED: u8 = 130;
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::parse();
    let style = Style::for_stdout(cli.global.color);
    match dispatch(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(msg) => {
            eprintln!("{} {msg}", style.fail("错误"));
            ExitCode::from(exit::USAGE)
        }
    }
}

async fn dispatch(cli: Cli) -> Result<u8, String> {
    // help / version 由 clap 在解析期直接处理并退出，走不到这里
    let Cli { global, command, .. } = cli;
    let Some(command) = command else {
        // arg_required_else_help 已经挡住了这条路，留着是为了不 panic
        Cli::command().print_help().ok();
        return Ok(exit::USAGE);
    };

    match command {
        Command::Run(args) => cmd_run(&global, args).await,
        Command::Ls(args) => cmd_ls(&global, args),
        Command::Check(args) => cmd_check(&global, args),
        Command::Show(args) => cmd_show(&global, args),
        Command::Env(args) => cmd_env(&global, args),
        Command::Cookie(args) => cmd_cookie(&global, args),
        Command::Report(args) => cmd_report(&global, args),
        Command::New(args) => cmd_new(&global, args),
        Command::Init(args) => cmd_init(&global, args),
        Command::Docs(args) => cmd_docs(&global, args),
        Command::Mcp(_) => cmd_mcp(&global).await,
        Command::Completion(args) => {
            clap_complete::generate(args.shell, &mut Cli::command(), "apicase", &mut std::io::stdout());
            Ok(exit::OK)
        }
    }
}

// ── run ─────────────────────────────────────────────

async fn cmd_run(g: &GlobalOpts, args: cli::RunArgs) -> Result<u8, String> {
    let stdin_case = args.targets.iter().any(|t| t == "-");
    let paths: Vec<PathBuf> =
        args.targets.iter().filter(|t| *t != "-").map(PathBuf::from).collect();
    if stdin_case && !paths.is_empty() {
        return Err("`-`（标准输入）不能与其它目标同时给：一次只跑一份内联用例".into());
    }

    let ws = resolve_workspace(g, paths.first().map(PathBuf::as_path))?;
    let fmt = output_format(g);
    let style = Style::for_stdout(g.color);
    let sink = report_sink(&args);

    let mut req = RunRequest {
        targets: paths,
        content: stdin_case.then(read_stdin).transpose()?,
        env: g.env.clone(),
        steps: args.steps,
        vars: parse_vars(&args.vars)?,
        concurrency: args.concurrency,
        stop_on_failure: args.bail,
        continue_on_assertion_failure: args.continue_on_assertion_failure.then_some(true),
        timeout_ms: args.timeout,
        insecure: args.insecure,
        recursive: !args.no_recursive,
        report: sink,
        proxy: resolve_proxy(),
    };
    // 内联用例没有落点可推导，除非显式给了 --report
    if req.content.is_some() && req.report == ReportSink::Auto {
        req.report = ReportSink::None;
    }

    // 进度：text 模式打到 stdout（它就是报告本身），json 模式打到 stderr（别脏了那份 JSON）
    let ws_info = ws.info();
    let on_event: Option<OnEvent> = (!g.quiet).then(|| {
        let (style, to_stderr) = (style, fmt == Format::Json);
        let emit = move |text: String| {
            if to_stderr {
                eprint!("{text}");
            } else {
                print!("{text}");
            }
        };
        Arc::new(move |ev: ops::run::Event<'_>| match ev {
            ops::run::Event::Start { total, environment } => {
                emit(render::run_header(&ws_info, environment, total, &style))
            }
            ops::run::Event::Case(c) => emit(render::case_line(c, &style, 40)),
        }) as OnEvent
    });

    let cancel = Cancel::new();
    watch_interrupt(cancel.clone());
    let outcome = ops::run(&ws, &req, on_event, cancel.clone()).await?;

    match fmt {
        Format::Text => {
            print!("{}", render::run_summary(&outcome.report, outcome.report_path.as_deref(), &style));
        }
        Format::Json => {
            let report = shrink(&outcome.report, args.detail);
            println!("{}", to_json(&report, outcome.report_path.as_deref())?);
        }
    }

    Ok(if cancel.is_cancelled() {
        exit::INTERRUPTED
    } else if outcome.has_errors() {
        exit::ERROR
    } else if outcome.has_failures() {
        exit::FAILED
    } else {
        exit::OK
    })
}

/// `--report` / `--no-report` / 默认。
///
/// **CLI 默认落盘**（跑完有报告，和界面一致），MCP 侧默认不落——AI 高频调用时
/// 每次甩一份几 MB 的 HTML 进工作空间是污染。默认值分化写在方案里，是刻意的。
fn report_sink(args: &cli::RunArgs) -> ReportSink {
    match (&args.report, args.no_report) {
        (Some(p), _) => ReportSink::Path(p.clone()),
        (None, true) => ReportSink::None,
        (None, false) => ReportSink::Auto,
    }
}

/// Ctrl-C：置取消位而不是直接死。
///
/// 取消在 case / step 边界生效，**已发出的 HTTP 不中断**——半截请求对被测服务是一种伤害，
/// 而等一个请求收尾最多几秒。收尾时那份「跑了一半」的报告仍会落盘。
/// 第二次 Ctrl-C 走系统默认（真的等不及了）。
fn watch_interrupt(cancel: Cancel) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n收到中断，正在收尾当前请求…（再按一次强制退出）");
            cancel.cancel();
            // 第二次交还给默认处理
            let _ = tokio::signal::ctrl_c().await;
            std::process::exit(exit::INTERRUPTED as i32);
        }
    });
}

/// 按详略裁剪报告。
///
/// **schema 不变、计数不变**，只是把体积大头拿掉——AI 与 `jq` 认的仍是同一套结构。
/// 具体地：通过的 step 去掉报文与断言明细（都通过了，没有可看的），
/// 未通过的 step 保留报文（截到 2KB）与失败的那几条断言——那才是排查现场。
fn shrink(r: &RunReport, detail: Detail) -> RunReport {
    if detail == Detail::Full {
        return r.clone();
    }
    const KEEP_BYTES: usize = 2 * 1024;
    let mut out = r.clone();
    for c in &mut out.cases {
        for s in &mut c.steps {
            if s.status == StepStatus::Passed {
                s.request = None;
                s.response = None;
                s.assertions.clear();
            } else {
                s.assertions.retain(|a| !a.ok);
                if let Some(req) = s.request.as_mut() {
                    req.body = clip(&req.body, KEEP_BYTES);
                }
                if let Some(resp) = s.response.as_mut() {
                    resp.body = clip(&resp.body, KEEP_BYTES);
                }
            }
        }
    }
    out
}

fn clip(b: &apicase_core::report::BodyRecord, max: usize) -> apicase_core::report::BodyRecord {
    let mut out = apicase_core::report::BodyRecord::clip(b.preview.as_deref(), max);
    // `bytes` 要保持原始大小——截断后仍能看出响应到底多大
    out.bytes = b.bytes;
    out.truncated = out.truncated || b.truncated;
    out
}

/// 报告 JSON 外挂一个 `reportFile`：跑完之后最常问的下一句就是「那份 HTML 在哪」。
fn to_json(report: &RunReport, path: Option<&Path>) -> Result<String, String> {
    let mut v = serde_json::to_value(report).map_err(|e| format!("序列化报告失败：{e}"))?;
    if let (Some(obj), Some(p)) = (v.as_object_mut(), path) {
        obj.insert("reportFile".into(), serde_json::Value::String(p.to_string_lossy().into_owned()));
    }
    serde_json::to_string_pretty(&v).map_err(|e| format!("序列化报告失败：{e}"))
}

// ── ls / check / show ───────────────────────────────

fn cmd_ls(g: &GlobalOpts, args: cli::LsArgs) -> Result<u8, String> {
    let ws = resolve_workspace(g, args.targets.first().map(PathBuf::as_path))?;
    let items = ops::list(&ws, &args.targets, !args.no_recursive, args.query.as_deref());
    match output_format(g) {
        Format::Text => print!("{}", render::list_table(&items, &Style::for_stdout(g.color))),
        Format::Json => println!("{}", json_pretty(&items)?),
    }
    Ok(exit::OK)
}

fn cmd_check(g: &GlobalOpts, args: cli::CheckArgs) -> Result<u8, String> {
    let from_stdin = args.targets.iter().any(|t| t == "-");
    let paths: Vec<PathBuf> = args.targets.iter().filter(|t| *t != "-").map(PathBuf::from).collect();
    let ws = resolve_workspace(g, paths.first().map(PathBuf::as_path))?;

    let report = if from_stdin {
        let text = read_stdin()?;
        CheckReport::of_one(ops::check_text(&text, ops::run::STDIN_FILE))
    } else {
        ops::check(&ws, &paths, !args.no_recursive)
    };

    match output_format(g) {
        Format::Text => print!("{}", render::check_text(&report, &Style::for_stdout(g.color))),
        Format::Json => println!("{}", json_pretty(&report)?),
    }
    // 警告不该把 CI 卡住，只有 error 才算没过
    Ok(if report.errors > 0 { exit::ERROR } else { exit::OK })
}

fn cmd_show(g: &GlobalOpts, args: cli::ShowArgs) -> Result<u8, String> {
    let text = std::fs::read_to_string(&args.target)
        .map_err(|e| format!("读取 {} 失败：{e}", args.target.display()))?;
    let analyzed = apicase_core::yaml::analyze_case(&text);
    let case = analyzed
        .case
        .filter(|_| analyzed.valid)
        .ok_or_else(|| analyzed.error.unwrap_or_else(|| "不是有效的用例".into()))?;
    let _ = g;
    match args.r#as {
        // 规范化后的 YAML：与保存时落盘的一模一样，可用来看「格式化会改动什么」
        cli::ShowAs::Yaml => print!("{}", apicase_core::yaml::dump_case(&case)),
        cli::ShowAs::Json => println!("{}", json_pretty(&case)?),
    }
    Ok(exit::OK)
}

// ── env ─────────────────────────────────────────────

fn cmd_env(g: &GlobalOpts, args: cli::EnvArgs) -> Result<u8, String> {
    let ws = resolve_workspace(g, None)?;
    let style = Style::for_stdout(g.color);
    let json = output_format(g) == Format::Json;

    match args.command.unwrap_or(cli::EnvCommand::Ls) {
        cli::EnvCommand::Ls => {
            let names = ws.env_names();
            let default = ws.default_env();
            if json {
                println!("{}", json_pretty(&serde_json::json!({ "environments": names, "default": default }))?);
            } else if names.is_empty() {
                println!("{}", style.dim("没有配置任何环境（application.yml 的 environment 键）"));
            } else {
                for n in &names {
                    let mark = if *n == default { style.pass(" (默认)") } else { String::new() };
                    println!("{n}{mark}");
                }
            }
        }
        cli::EnvCommand::Show { name } => {
            let name = name.or_else(|| g.env.clone());
            let info = ws.env_info(name.as_deref());
            if let Some(n) = name.as_deref() {
                if !ws.env_names().iter().any(|e| e == n) {
                    return Err(format!("没有名为 {n} 的环境（现有：{}）", ws.env_names().join("、")));
                }
            }
            if json {
                println!("{}", json_pretty(&info)?);
            } else {
                println!("{} {}", style.dim("环境"), info.name);
                for (k, v) in &info.vars {
                    println!("  {} {}", style::pad(k, 20), v);
                }
            }
        }
    }
    Ok(exit::OK)
}

// ── cookie ──────────────────────────────────────────

fn cmd_cookie(g: &GlobalOpts, args: cli::CookieArgs) -> Result<u8, String> {
    let ws = resolve_workspace(g, None)?;
    // cookie 会话是「这个项目的登录态」，未锚定的目录里没有这回事——
    // 直接说清楚，好过显示一个空 jar 让人以为「登录态丢了」
    if ws.is_scratch() {
        return Err(format!(
            "{} 不是工作空间（没有 {}），没有 cookie 会话——\
             用 apicase init 把它声明为工作空间，或用 -w 指定",
            ws.root.display(),
            apicase_core::workspace::CONFIG_FILE
        ));
    }
    let jar = apicase_core::cookie::jar_at(Some(&ws.jar_path().to_string_lossy()));
    let style = Style::for_stdout(g.color);
    let json = output_format(g) == Format::Json;

    match args.command.unwrap_or(cli::CookieCommand::Ls { domain: None }) {
        cli::CookieCommand::Ls { domain } => {
            let all = jar.list();
            let items: Vec<_> = all
                .into_iter()
                .filter(|c| domain.as_deref().is_none_or(|d| c.domain.contains(d)))
                .collect();
            if json {
                println!("{}", json_pretty(&items)?);
            } else if items.is_empty() {
                println!("{}", style.dim("jar 里没有 cookie"));
            } else {
                for c in &items {
                    println!(
                        "{} {} {} {}",
                        style::pad(&c.domain, 28),
                        style::pad(&c.path, 10),
                        style::pad(&c.name, 20),
                        c.value
                    );
                }
                println!("{}", style.dim(&format!("\n{} 条", items.len())));
            }
        }
        cli::CookieCommand::Rm { domain, path, name } => {
            // 主键是 domain + path + name：只按 name 删会误伤同名不同域的那条
            if !jar.remove(&domain, &path, &name) {
                return Err(format!("jar 里没有 {domain} {path} {name}"));
            }
            apicase_core::cookie::flush_all();
            println!("已删除 {domain} {path} {name}");
        }
        cli::CookieCommand::Clear { domain } => {
            let n = jar.clear(domain.as_deref());
            apicase_core::cookie::flush_all();
            println!("已清空 {n} 条{}", domain.map(|d| format!("（{d}）")).unwrap_or_default());
        }
    }
    Ok(exit::OK)
}

// ── report ──────────────────────────────────────────

fn cmd_report(g: &GlobalOpts, args: cli::ReportArgs) -> Result<u8, String> {
    let ws = resolve_workspace(g, None)?;
    let style = Style::for_stdout(g.color);
    let json = output_format(g) == Format::Json;

    match args.command.unwrap_or(cli::ReportCommand::Ls) {
        cli::ReportCommand::Ls => {
            let files = report_files(&ws);
            if json {
                let list: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                println!("{}", json_pretty(&list)?);
            } else if files.is_empty() {
                println!("{}", style.dim("还没有报告（跑一次 apicase run 就有了）"));
            } else {
                for p in &files {
                    println!("{}", ws.rel(p));
                }
            }
        }
        cli::ReportCommand::Show { file, filter, detail } => {
            let path = pick_report(&ws, file)?;
            let text = std::fs::read_to_string(&path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
            let mut report = apicase_core::render::parse_report_html(&text)
                .ok_or_else(|| format!("{} 不是 apicase 生成的报告", path.display()))?;
            report.cases.retain(|c| match filter {
                cli::ReportFilter::All => true,
                cli::ReportFilter::Failed => c.status == apicase_core::report::CaseStatus::Failed,
                cli::ReportFilter::Error => c.status == apicase_core::report::CaseStatus::Error,
                cli::ReportFilter::Bad => !matches!(
                    c.status,
                    apicase_core::report::CaseStatus::Passed | apicase_core::report::CaseStatus::Running
                ),
            });
            if json {
                println!("{}", to_json(&shrink(&report, detail), Some(&path))?);
            } else {
                let col = render::name_column(report.cases.iter().map(|c| c.file.clone()));
                for c in &report.cases {
                    print!("{}", render::case_line(c, &style, col));
                }
                print!("{}", render::run_summary(&report, Some(&path), &style));
            }
        }
        cli::ReportCommand::Open { file } => {
            let path = pick_report(&ws, file)?;
            open_in_system(&path)?;
        }
    }
    Ok(exit::OK)
}

/// 历史报告，**新的在前**（找的通常是刚跑的那份）。
fn report_files(ws: &Workspace) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(ws.reports_dir()) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    // 文件名以 YYYYMMDDHHmmss 开头，字典序即时序——不必去读 mtime
    v.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    v
}

fn pick_report(ws: &Workspace, file: Option<PathBuf>) -> Result<PathBuf, String> {
    match file {
        Some(p) if p.is_file() => Ok(p),
        Some(p) => {
            // 允许只写文件名：报告都在同一个目录里，写全路径是负担
            let in_dir = ws.reports_dir().join(&p);
            in_dir.is_file().then_some(in_dir).ok_or_else(|| format!("找不到报告：{}", p.display()))
        }
        None => report_files(ws).into_iter().next().ok_or_else(|| "还没有任何报告".to_string()),
    }
}

/// 用系统默认程序打开。三个平台三条命令，不引 opener 库——就这一处用。
fn open_in_system(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");

    cmd.arg(path);
    cmd.status().map_err(|e| format!("打开 {} 失败：{e}", path.display()))?;
    Ok(())
}

// ── new / init / docs / mcp ─────────────────────────

fn cmd_new(g: &GlobalOpts, args: cli::NewArgs) -> Result<u8, String> {
    let mut path = args.path;
    if path.extension().is_none() {
        path.set_extension("yml");
    }
    if path.exists() && !args.force {
        return Err(format!("{} 已存在（要覆盖加 --force）", path.display()));
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = args.name.unwrap_or(stem);

    // 经模型再 dump，而不是拼字符串：格式（引号、缩进、字段顺序）由 core 的输出器说了算，
    // 新建的用例因此与保存后的一模一样，不会一打开就产生一次格式 diff
    let case = apicase_core::model::Case {
        version: apicase_core::model::CASE_VERSION.into(),
        name: (!name.is_empty()).then_some(name),
        vars: None,
        requests: vec![apicase_core::model::Step {
            id: "request".into(),
            protocol: "http".into(),
            ui: None,
            http: apicase_core::model::HttpSpec {
                method: args.method.to_uppercase(),
                url: args.url,
                ..Default::default()
            },
            depends_on: Vec::new(),
            outputs: Vec::new(),
            assertions: vec![apicase_core::model::Assertion {
                target: "res.status".into(),
                op: apicase_core::model::AssertOp::Eq,
                value: Some("200".into()),
            }],
            docs: None,
        }],
    };

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录 {} 失败：{e}", dir.display()))?;
    }
    std::fs::write(&path, apicase_core::yaml::dump_case(&case))
        .map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
    let _ = g;
    println!("{}", path.display());
    Ok(exit::OK)
}

fn cmd_init(g: &GlobalOpts, args: cli::InitArgs) -> Result<u8, String> {
    let dir = args.dir.or_else(|| g.workspace.clone()).unwrap_or_else(|| PathBuf::from("."));
    Workspace::init(&dir)?;
    let root = dir.canonicalize().unwrap_or(dir);
    println!("已初始化工作空间：{}", root.display());
    println!("{}", Style::for_stdout(g.color).dim("下一步：apicase new 用例名 && apicase run"));
    Ok(exit::OK)
}

/// 格式规范。与 MCP 的 `apicase_docs` 同一份内容——
/// 人和 AI 查的是同一份规范，不该有「文档」和「给 AI 的文档」两种说法。
fn cmd_docs(g: &GlobalOpts, args: cli::DocsArgs) -> Result<u8, String> {
    if args.topics {
        let style = Style::for_stdout(g.color);
        for t in docs::TOPICS {
            println!("{} {}", style::pad(t.name, 12), style.dim(t.about));
        }
        return Ok(exit::OK);
    }
    match output_format(g) {
        Format::Text => print!("{}", docs::topic(args.topic.as_deref())),
        Format::Json => println!(
            "{}",
            json_pretty(&serde_json::json!({
                "topic": args.topic.clone().unwrap_or_else(|| "case".into()),
                "topics": docs::topic_list(),
                "markdown": docs::topic(args.topic.as_deref()),
            }))?
        ),
    }
    Ok(exit::OK)
}

/// 以 MCP 服务器运行（stdio）。跑到 stdin 关闭为止。
///
/// 工作空间在这里就定下来：`-w` 指定，或留空让每次工具调用按调用方的工作目录推导。
/// 后者是给「一个 Agent 会话里切换多个项目」留的口子，配置时写死 `-w` 更稳。
async fn cmd_mcp(g: &GlobalOpts) -> Result<u8, String> {
    let root = g
        .workspace
        .clone()
        .or_else(|| std::env::var_os("APICASE_WORKSPACE").map(PathBuf::from))
        .or_else(|| Workspace::find(&cwd()));
    mcp::serve(root).await?;
    Ok(exit::OK)
}

// ── 公共 ────────────────────────────────────────────

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 定位工作空间：`-w` → `APICASE_WORKSPACE` → 从目标（或当前目录）**向上找** `application.yml`。
///
/// 从目标而非当前目录起算，是为了让 `apicase run /别的项目/api/login.yml` 自动用
/// 那个项目的环境与设置——跑哪个文件就该用哪个项目的配置，这比"当前目录是什么"更符合意图。
fn resolve_workspace(g: &GlobalOpts, hint: Option<&Path>) -> Result<Workspace, String> {
    if let Some(w) = &g.workspace {
        return Workspace::open(w);
    }
    if let Some(w) = std::env::var_os("APICASE_WORKSPACE") {
        return Workspace::open(PathBuf::from(w));
    }
    let start = hint.map(Path::to_path_buf).unwrap_or_else(cwd);
    let start = start.canonicalize().unwrap_or(start);
    Workspace::discover(&start)
}

/// 输出格式：显式指定的最大，否则跟着「stdout 是不是终端」走。
///
/// 管道里默认给 JSON 是为了 `apicase run | jq` 直接成立；终端里默认给文本，
/// 因为那时读的是人。
fn output_format(g: &GlobalOpts) -> Format {
    if g.json {
        return Format::Json;
    }
    g.format.unwrap_or({
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            Format::Text
        } else {
            Format::Json
        }
    })
}

/// `--var name=value`。**只按第一个 `=` 切**——值里带 `=` 是常态（base64、JWT、query 串）。
fn parse_vars(raw: &[String]) -> Result<Vec<(String, String)>, String> {
    raw.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.to_string()))
                .filter(|(k, _)| !k.is_empty())
                .ok_or_else(|| format!("--var 要写成 name=value，收到的是 `{s}`"))
        })
        .collect()
}

/// 代理，两级回落：
///
/// 1. **`APICASE_PROXY` 环境变量**（`none` 直连 / `custom` + `APICASE_PROXY_URL` 指定）——
///    CI 与容器里没有 `settings.json`，而那里恰恰需要显式控制，故环境变量压过一切。
/// 2. **界面的应用设置**（`settings.json` 的 `proxy`）——本机开发时自动与界面一致。
///    不这么做就会出现「界面里设了直连、CLI 照样走系统代理」，
///    也就是「界面里跑过了、CLI 跑却挂了」，正是整套架构最想避免的那种事。
/// 3. 都没有 → 跟随系统（reqwest 的默认行为）。
///
/// 第 2 步**读不到一律当作没配过**：只用 CLI 的人（CI、容器）根本没有那个文件，
/// 那不是错误状态。
fn resolve_proxy() -> Option<apicase_core::http::ProxyConfig> {
    if let Ok(mode) = std::env::var("APICASE_PROXY") {
        return Some(apicase_core::http::ProxyConfig {
            url: std::env::var("APICASE_PROXY_URL").ok(),
            mode,
        });
    }
    apicase_core::paths::load_app_prefs().proxy
}

fn read_stdin() -> Result<String, String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).map_err(|e| format!("读取标准输入失败：{e}"))?;
    if buf.trim().is_empty() {
        return Err("标准输入是空的".into());
    }
    Ok(buf)
}

fn json_pretty<T: serde::Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string_pretty(v).map_err(|e| format!("序列化失败：{e}"))
}

/// 本地时间的 `YYYYMMDDHHmmss`，用作报告文件名前缀。
///
/// 格式由 core 定义（`util::stamp14` + `workspace::report_file_name`），
/// 这里只负责把「本地偏移」加进去——core 不引时区库，而这个名字是给人看的，
/// UTC 会让下午三点跑的那份写着 07。
pub fn local_stamp() -> String {
    let offset_ms = chrono::Local::now().offset().local_minus_utc() as i64 * 1000;
    let ms = apicase_core::util::now_ms() as i64 + offset_ms;
    apicase_core::util::stamp14(ms.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vars_split_on_the_first_equals_only() {
        let got = parse_vars(&["a=1".into(), "token=ab=cd==".into(), " k =v".into()]).expect("应能解析");
        assert_eq!(
            got,
            vec![
                ("a".into(), "1".into()),
                ("token".into(), "ab=cd==".into()),
                ("k".into(), "v".into())
            ],
            "值里的 = 要原样保留（base64 / JWT 里全是）"
        );
        assert!(parse_vars(&["没有等号".into()]).is_err());
        assert!(parse_vars(&["=光有值".into()]).is_err());
    }

    #[test]
    fn local_stamp_is_fourteen_digits() {
        let s = local_stamp();
        assert_eq!(s.len(), 14, "{s}");
        assert!(s.chars().all(|c| c.is_ascii_digit()), "{s}");
    }

    /// 摘要模式：schema 与计数不变，只是把体积大头拿掉
    #[test]
    fn shrink_keeps_the_shape_and_the_failing_evidence() {
        use apicase_core::report::*;
        let mut r = RunReport {
            schema_version: REPORT_SCHEMA_VERSION,
            tool: ToolInfo { name: "apicase".into(), version: "0".into() },
            started_at: "2026-08-03T00:00:00.000Z".into(),
            finished_at: None,
            duration_ms: 1,
            status: RunStatus::Done,
            workspace: WorkspaceInfo { name: "w".into(), root: "/w".into() },
            environment: EnvironmentInfo { name: "dev".into(), vars: Default::default() },
            options: RunOptions {
                targets: vec![],
                recursive: true,
                environment: "dev".into(),
                concurrency: 1,
                stop_on_failure: false,
                max_body_bytes: 65536,
                continue_on_assertion_failure: false,
            },
            summary: RunSummary::default(),
            cases: vec![],
        };
        let body = |s: &str| BodyRecord { preview: Some(s.into()), bytes: s.len(), truncated: false };
        let resp = |s: &str| ResponseRecord {
            status: 200,
            status_text: "OK".into(),
            headers: vec![],
            body: body(s),
            elapsed_ms: 1,
        };
        let mk = |id: &str, st: StepStatus, ok: bool| StepResult {
            id: id.into(),
            status: st,
            duration_ms: 1,
            request: None,
            response: Some(resp(&"x".repeat(10_000))),
            outputs: Default::default(),
            assertions: vec![AssertRecord {
                target: "res.status".into(),
                op: "eq".into(),
                expected: "200".into(),
                actual: "500".into(),
                ok,
            }],
            error: None,
            skip_reason: None,
        };
        r.cases = vec![CaseResult {
            file: "a.yml".into(),
            name: "a".into(),
            status: CaseStatus::Failed,
            skip_reason: None,
            started_at: "2026-08-03T00:00:00.000Z".into(),
            duration_ms: 2,
            steps: vec![mk("ok", StepStatus::Passed, true), mk("bad", StepStatus::Failed, false)],
        }];
        r.summary = RunSummary::of(&r.cases);

        let small = shrink(&r, Detail::Summary);
        assert_eq!(small.summary, r.summary, "统计不变");
        assert_eq!(small.cases[0].steps.len(), 2, "step 数不变，计数才对得上");

        let passed = &small.cases[0].steps[0];
        assert!(passed.response.is_none(), "通过的 step 不留报文");
        assert!(passed.assertions.is_empty(), "通过的断言没有可看的");

        let failed = &small.cases[0].steps[1];
        let kept = failed.response.as_ref().expect("失败的 step 要留现场");
        assert_eq!(kept.body.bytes, 10_000, "原始大小要保住");
        assert!(kept.body.truncated && kept.body.preview.as_ref().unwrap().len() <= 2048);
        assert_eq!(failed.assertions.len(), 1, "失败的断言留着");

        assert_eq!(shrink(&r, Detail::Full), r, "full 原样返回");
    }
}
