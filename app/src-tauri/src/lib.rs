//! apicase 桌面壳。
//!
//! **这个 crate 里没有一行执行语义。** case 怎么解析、变量怎么代入、请求怎么组装、
//! 认证怎么走、断言怎么判、报告怎么渲染——全在 `apicase-core`。留在这里的只有
//! 桌面应用才需要的东西：文件树、文件系统监听、伪终端、应用配置目录。
//!
//! 这样分的理由是**将来的 `apicase run` CLI**：它会平行地依赖同一个 core，
//! 因此不存在"界面里跑过了、CLI 跑却挂了"这类两套实现必然产生的漂移。
//! 反过来说，任何被写进本 crate 的执行逻辑，都是 CLI 将来要重写一遍的技术债。

mod commands;

use commands::{ai, app, cookies, exec, fs, terminal, watch};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(watch::WatchState::default())
        .manage(terminal::PtyState::default())
        .manage(exec::RunState::default())
        .invoke_handler(tauri::generate_handler![
            // 执行（全部转发给 apicase-core）
            exec::analyze_case,
            exec::dump_case,
            exec::parse_app_config,
            exec::dump_app_config,
            exec::run_step,
            exec::topo_order,
            exec::blocked_steps,
            exec::run_batch,
            exec::cancel_run,
            exec::report_shell,
            exec::parse_report,
            // Cookie jar（设置页「Cookies」）
            cookies::list_cookies,
            cookies::save_cookie,
            cookies::delete_cookie,
            cookies::clear_cookies,
            // 设置页「AI」：命令行工具接进 PATH / 生成 AGENTS.md
            ai::ai_status,
            ai::ai_install_cli,
            ai::ai_write_agents,
            // 应用级
            app::init_workspace,
            app::startup_workspace,
            app::read_app_settings,
            app::write_app_settings,
            app::app_paths,
            app::list_cert_files,
            app::system_info,
            // 文件与目录
            fs::list_dir,
            fs::read_text_file,
            fs::read_file_smart,
            fs::write_text_file,
            fs::create_file,
            fs::create_dir,
            fs::rename_path,
            fs::copy_path,
            fs::delete_path,
            fs::search_workspace,
            fs::path_exists,
            // 文件系统监听
            watch::watch_workspace,
            // 伪终端
            terminal::terminal_open,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_close,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
