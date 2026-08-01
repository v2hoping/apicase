//! 无界面地跑完一个工作空间，并生成 HTML 报告。
//!
//! 这是 **`apicase run` CLI 的可行性验证**：整段代码没有一处 Tauri、没有一处 React，
//! 只调 `apicase-core`。CLI 真正落地时要补的是参数解析、输出格式与退出码，
//! 执行与报告这两件事在这里已经是完整的了。
//!
//! ```sh
//! cargo run -p apicase-core --example run_workspace -- <工作空间> [环境名]
//! ```

use apicase_core::report::{RunOptions, RunStatus, WorkspaceInfo};
use apicase_core::runner::{self, BatchMeta, BatchTarget, Cancel, RunOpts};
use apicase_core::{http, render, yaml};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().ok_or("用法：run_workspace <工作空间> [环境名]")?);
    let env_name = args.next().unwrap_or_else(|| "default".into());

    // ① 读工作空间配置
    let app_yml = std::fs::read_to_string(root.join("application.yml")).unwrap_or_default();
    let envs = yaml::parse_environments(&app_yml);
    let settings = yaml::parse_settings(&app_yml);
    let vars = yaml::env_vars(&envs, &env_name);
    println!("工作空间：{}", root.display());
    println!("环境：{env_name}（{} 个变量）", vars.len());

    // ② 发现用例：.yml / .yaml，排除 application.yml，按路径排序（顺序可预期）
    let mut files = Vec::new();
    discover(&root, &mut files);
    files.sort();
    if files.is_empty() {
        println!("没有找到可运行的用例");
        return Ok(());
    }
    println!("发现 {} 个用例\n", files.len());

    let targets: Vec<BatchTarget> = files
        .iter()
        .map(|p| BatchTarget {
            file: p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/"),
            path: p.to_string_lossy().into_owned(),
        })
        .collect();

    // ③ 执行参数：CA 的相对路径在此还原为绝对路径（存盘用相对是为了随 git 走）
    let mut opts = RunOpts::for_batch(apicase_core::report::EnvironmentInfo { name: env_name.clone(), vars });
    opts.client = http::ClientConfig {
        // 代理：默认跟随系统；`APICASE_PROXY=none` 直连（打本机服务时必须，
        // 否则 macOS 的系统代理设置会把 127.0.0.1 的请求也劫走）。
        proxy: std::env::var("APICASE_PROXY").ok().map(|mode| http::ProxyConfig {
            url: std::env::var("APICASE_PROXY_URL").ok(),
            mode,
        }),
        options: Some(http::RequestOptions {
            verify_ssl: (!settings.verify_ssl).then_some(false),
            ca_cert_path: (settings.use_custom_ca && !settings.ca_cert.is_empty())
                .then(|| root.join(&settings.ca_cert).to_string_lossy().into_owned()),
            timeout_ms: (settings.timeout_ms > 0).then_some(settings.timeout_ms),
        }),
    };

    let meta = BatchMeta {
        workspace: WorkspaceInfo {
            name: root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            root: root.to_string_lossy().into_owned(),
        },
        tool_version: env!("CARGO_PKG_VERSION").into(),
        options: RunOptions {
            targets: vec![".".into()],
            recursive: true,
            environment: env_name,
            concurrency: opts.concurrency,
            stop_on_failure: opts.stop_on_failure,
            redact: opts.redact,
            max_body_bytes: opts.max_body_bytes,
        },
    };

    // ④ 跑，每完成一个 case 打一行。
    // 回调在开跑与收尾时也会来（那两次 cases 没有新增），靠计数区分，
    // 否则最后一个 case 会被打印两遍。
    let seen = std::sync::atomic::AtomicUsize::new(0);
    let progress = Arc::new(move |r: &apicase_core::report::RunReport| {
        let n = r.cases.len();
        if n <= seen.swap(n, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if let Some(c) = r.cases.last() {
            let mark = match c.status {
                apicase_core::report::CaseStatus::Passed => "✓",
                apicase_core::report::CaseStatus::Failed => "✕",
                apicase_core::report::CaseStatus::Error => "!",
                apicase_core::report::CaseStatus::Skipped => "–",
                apicase_core::report::CaseStatus::Running => "·",
            };
            println!("  {mark} {} ({}ms){}", c.file, c.duration_ms, c.skip_reason.as_deref().unwrap_or(""));
        }
    });
    let report = runner::run_batch(targets, meta, opts, Some(progress), Cancel::new()).await;

    // ⑤ 落一份与桌面端**逐字相同**的报告（同一个目录、同样的单文件形态；
    //    桌面端文件名带时间戳，这里用固定名，重跑覆盖即可）
    let out_dir = root.join(".apicase/reports");
    std::fs::create_dir_all(&out_dir)?;
    let out = out_dir.join("cli.html");
    std::fs::write(&out, render::render_html(&report))?;

    let s = report.summary;
    println!(
        "\n总计 {} · 通过 {} · 失败 {} · 错误 {} · 跳过 {}（断言 {}/{}）",
        s.total, s.passed, s.failed, s.error, s.skipped, s.assertions.passed, s.assertions.total
    );
    println!("耗时 {}ms · 报告 {}", report.duration_ms, out.display());

    // CI 要靠退出码判定成败
    let ok = report.status == RunStatus::Done && s.failed == 0 && s.error == 0;
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// 递归发现用例：跳过隐藏项与 `node_modules` / `target` / `dist`（同文件树的约定）。
fn discover(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "dist") {
            continue;
        }
        let p = ent.path();
        if p.is_dir() {
            discover(&p, out);
        } else if matches!(p.extension().and_then(|e| e.to_str()), Some("yml") | Some("yaml"))
            && name != "application.yml"
        {
            out.push(p);
        }
    }
}
