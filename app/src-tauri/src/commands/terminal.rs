//! 伪终端（底部终端栏）。
//!
//! 真·交互式终端：`portable-pty` 起一个系统 shell，输出经后台线程以事件流回前端，
//! 前端 xterm.js 渲染；输入 / 尺寸变化再经命令写回。与 VSCode 内置终端体验一致。

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

// 前端 xterm.js 渲染；输入 / 尺寸变化再经命令写回。与 VSCode 内置终端体验一致。

/// 一个伪终端会话：master 用于 resize、writer 用于写入用户输入、child 用于关闭时 kill。
struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

/// 终端会话的托管状态：终端 id → 会话（前端可开多个终端；本期 UI 用单个）。
#[derive(Default)]
pub struct PtyState(Mutex<HashMap<String, PtySession>>);

/// Tauri 命令：打开伪终端，在 `cwd` 起交互 shell；后台线程把输出以 `terminal://data/{id}`
/// 事件（原始字节）发往前端，进程结束发 `terminal://exit/{id}`。
#[tauri::command]
pub fn terminal_open(
    app: AppHandle,
    state: State<PtyState>,
    id: String,
    cwd: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("创建伪终端失败: {e}"))?;

    #[cfg(windows)]
    let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
    #[cfg(not(windows))]
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());

    let mut cmd = CommandBuilder::new(shell);
    if Path::new(&cwd).is_dir() {
        cmd.cwd(&cwd);
    }
    // 让 shell 内程序输出全彩、并被识别为交互式终端
    cmd.env("TERM", "xterm-256color");
    // macOS：不要继承外层 Terminal.app 的会话标识（TERM_PROGRAM=Apple_Terminal /
    // TERM_SESSION_ID）。否则 zsh 会误触发 /etc/zshrc_Apple_Terminal 的「会话保存/恢复」，
    // 与外层窗口共用同一 session 文件发生竞态，把 "Saving session..." 状态文本写进
    // ~/.zsh_sessions/<id>.session，下次 source 时当命令执行（command not found: Saving）。
    // 本终端并非 Apple Terminal，改用自有标识并显式禁用该机制即可根除。
    cmd.env("TERM_PROGRAM", "apicase");
    cmd.env_remove("TERM_SESSION_ID");
    cmd.env_remove("TERM_PROGRAM_VERSION");
    cmd.env("SHELL_SESSIONS_DISABLE", "1");

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("启动 shell 失败: {e}"))?;
    // slave 端在 spawn 后即可释放：留着它会让子进程退出时 master 读端拿不到 EOF
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("读取终端失败: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("写入终端失败: {e}"))?;

    let data_event = format!("terminal://data/{id}");
    let exit_event = format!("terminal://exit/{id}");
    // 输出读取线程：阻塞读 master，成块以事件发往前端；EOF / 出错上报退出后退出线程。
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = app.emit(&data_event, buf[..n].to_vec());
                }
                Err(_) => break,
            }
        }
        let _ = app.emit(&exit_event, ());
    });

    state.0.lock().unwrap().insert(
        id,
        PtySession {
            master: pair.master,
            writer,
            child,
        },
    );
    Ok(())
}

/// Tauri 命令：把用户输入（xterm onData 的字符串）写入终端。
#[tauri::command]
pub fn terminal_write(state: State<PtyState>, id: String, data: String) -> Result<(), String> {
    let mut map = state.0.lock().unwrap();
    if let Some(sess) = map.get_mut(&id) {
        sess.writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("写入终端失败: {e}"))?;
        let _ = sess.writer.flush();
    }
    Ok(())
}

/// Tauri 命令：调整终端行列（面板 / 窗口尺寸变化时）。
#[tauri::command]
pub fn terminal_resize(state: State<PtyState>, id: String, cols: u16, rows: u16) -> Result<(), String> {
    let map = state.0.lock().unwrap();
    if let Some(sess) = map.get(&id) {
        sess.master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("调整终端尺寸失败: {e}"))?;
    }
    Ok(())
}

/// Tauri 命令：关闭终端会话（杀子进程并释放）。
#[tauri::command]
pub fn terminal_close(state: State<PtyState>, id: String) -> Result<(), String> {
    if let Some(mut sess) = state.0.lock().unwrap().remove(&id) {
        let _ = sess.child.kill();
    }
    Ok(())
}
