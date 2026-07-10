// apicase 后端：单 API 调试核心命令 send_request。
// 模型与执行逻辑分离 —— perform_request 不依赖 Tauri，可独立单元/集成测试。
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

/// 一对 HTTP 头（请求头 / 响应头通用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub key: String,
    pub value: String,
}

/// 前端传入的 API 请求 —— 即「单节点 DAG」的执行输入
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    #[serde(default)]
    pub body: Option<String>,
}

/// 返回给前端的响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<HeaderEntry>,
    pub body: String,
    pub elapsed_ms: u128,
}

/// 真正发起请求的逻辑（与 Tauri 解耦，便于测试）。
/// 由后端用 reqwest 发出，天然绕过浏览器 CORS。
async fn perform_request(req: ApiRequest) -> Result<ApiResponse, String> {
    let url = req.url.trim();
    if url.is_empty() {
        return Err("URL 不能为空".to_string());
    }

    let method = reqwest::Method::from_bytes(req.method.trim().to_uppercase().as_bytes())
        .map_err(|_| format!("非法的 HTTP 方法: {}", req.method))?;

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut builder = client.request(method, url);
    for h in &req.headers {
        if h.key.trim().is_empty() {
            continue;
        }
        builder = builder.header(h.key.trim(), h.value.as_str());
    }
    if let Some(body) = &req.body {
        if !body.is_empty() {
            builder = builder.body(body.clone());
        }
    }

    let start = Instant::now();
    let resp = builder
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;

    let status = resp.status();
    let status_text = status.canonical_reason().unwrap_or("").to_string();
    let headers: Vec<HeaderEntry> = resp
        .headers()
        .iter()
        .map(|(k, v)| HeaderEntry {
            key: k.to_string(),
            value: v.to_str().unwrap_or("").to_string(),
        })
        .collect();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("读取响应体失败: {e}"))?;
    let elapsed_ms = start.elapsed().as_millis();

    Ok(ApiResponse {
        status: status.as_u16(),
        status_text,
        headers,
        body,
        elapsed_ms,
    })
}

/// Tauri 命令：发送单个 API 请求
#[tauri::command]
async fn send_request(request: ApiRequest) -> Result<ApiResponse, String> {
    perform_request(request).await
}

/// Tauri 命令：把一个目录初始化为 apicase 工作空间。
/// 工作空间根需有 `application.yml`（工作空间配置文件）；若不存在则写入一份初始模板。
#[tauri::command]
fn init_workspace(path: String) -> Result<(), String> {
    let dir = std::path::Path::new(&path);
    if !dir.is_dir() {
        return Err(format!("目录不存在: {path}"));
    }
    let cfg = dir.join("application.yml");
    if !cfg.exists() {
        let content = "# apicase 工作空间配置\n\
# environment：支持多套环境，可切换（dev / test / prod…）\n\
environment:\n  default: {}\n";
        std::fs::write(&cfg, content).map_err(|e| format!("写入 application.yml 失败: {e}"))?;
    }
    Ok(())
}

/// 目录项（文件树节点）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

/// Tauri 命令：列出某目录下的直接子项（文件树懒加载用）。
/// 跳过隐藏项（`.` 开头，如 .git/.DS_Store）；目录在前，组内按名称（不区分大小写）排序。
#[tauri::command]
fn list_dir(path: String) -> Result<Vec<DirEntry>, String> {
    let dir = std::path::Path::new(&path);
    if !dir.is_dir() {
        return Err(format!("不是目录: {path}"));
    }
    let mut entries: Vec<DirEntry> = Vec::new();
    for ent in std::fs::read_dir(dir).map_err(|e| format!("读取目录失败: {e}"))? {
        let ent = ent.map_err(|e| format!("读取目录项失败: {e}"))?;
        let name = ent.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let p = ent.path();
        let is_dir = p.is_dir();
        entries.push(DirEntry {
            name,
            path: p.to_string_lossy().to_string(),
            is_dir,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Tauri 命令：读取文本文件内容（case 解析用）。
#[tauri::command]
fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("读取文件失败: {e}"))
}

/// 智能读取的结果：要么是文本，要么判定为二进制/不支持编码。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    /// 二进制或不受支持的文本编码 —— 前端应显示占位提示而非编辑器
    pub binary: bool,
    /// 文本内容（binary=true 时为 None）
    pub text: Option<String>,
}

/// Tauri 命令：读文件并判定文本/二进制（仿 VSCode）。
/// 规则：前 64KB 含 NUL 字节 → 二进制（提前返回，不读大文件）；否则整体验 UTF-8，失败即"不支持的编码"。
#[tauri::command]
fn read_file_smart(path: String) -> Result<FileContent, String> {
    use std::io::Read;
    const SNIFF: usize = 64 * 1024;
    let mut file = std::fs::File::open(&path).map_err(|e| format!("读取文件失败: {e}"))?;
    let mut buf = vec![0u8; SNIFF];
    let n = file.read(&mut buf).map_err(|e| format!("读取文件失败: {e}"))?;
    buf.truncate(n);
    // NUL 字节是二进制的强特征（UTF-16 文本的 ASCII 区也含 NUL，一并归为不支持编码）
    if buf.contains(&0) {
        return Ok(FileContent { binary: true, text: None });
    }
    // 无 NUL：读完剩余部分再整体验 UTF-8
    file.read_to_end(&mut buf).map_err(|e| format!("读取文件失败: {e}"))?;
    match String::from_utf8(buf) {
        Ok(text) => Ok(FileContent { binary: false, text: Some(text) }),
        Err(_) => Ok(FileContent { binary: true, text: None }),
    }
}

/// Tauri 命令：写入文本文件（保存 case；存在即覆盖）。
#[tauri::command]
fn write_text_file(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, content).map_err(|e| format!("写入文件失败: {e}"))
}

/// Tauri 命令：新建文件（拒绝覆盖已存在，用于新建 case）。
#[tauri::command]
fn create_file(path: String, content: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if p.exists() {
        return Err(format!("已存在: {path}"));
    }
    std::fs::write(p, content).map_err(|e| format!("新建文件失败: {e}"))
}

/// Tauri 命令：新建目录（用于新建 folder；拒绝覆盖已存在）。
#[tauri::command]
fn create_dir(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if p.exists() {
        return Err(format!("已存在: {path}"));
    }
    std::fs::create_dir(p).map_err(|e| format!("新建目录失败: {e}"))
}

/// Tauri 命令：重命名 / 移动路径（文件或目录）。
#[tauri::command]
fn rename_path(from: String, to: String) -> Result<(), String> {
    if !std::path::Path::new(&from).exists() {
        return Err(format!("源路径不存在: {from}"));
    }
    if std::path::Path::new(&to).exists() {
        return Err(format!("目标已存在: {to}"));
    }
    std::fs::rename(&from, &to).map_err(|e| format!("重命名失败: {e}"))
}

/// Tauri 命令：删除路径（文件用 remove_file，目录递归删除）。
#[tauri::command]
fn delete_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if p.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| format!("删除目录失败: {e}"))
    } else if p.exists() {
        std::fs::remove_file(p).map_err(|e| format!("删除文件失败: {e}"))
    } else {
        Err(format!("路径不存在: {path}"))
    }
}

/// Tauri 命令：在工作空间内递归搜索名称匹配（不区分大小写）的文件/目录（搜索栏用）。
/// 跳过隐藏项与常见大目录（node_modules/target/dist）；结果数上限 200。
#[tauri::command]
fn search_workspace(root: String, query: String) -> Result<Vec<DirEntry>, String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let root_path = std::path::Path::new(&root);
    if !root_path.is_dir() {
        return Err(format!("不是目录: {root}"));
    }
    const LIMIT: usize = 200;
    let mut out: Vec<DirEntry> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let p = ent.path();
            let is_dir = p.is_dir();
            if is_dir && name != "node_modules" && name != "target" && name != "dist" {
                stack.push(p.clone());
            }
            if name.to_lowercase().contains(&q) {
                out.push(DirEntry {
                    name,
                    path: p.to_string_lossy().to_string(),
                    is_dir,
                });
                if out.len() >= LIMIT {
                    return Ok(out);
                }
            }
        }
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// 文件监听的托管状态：持有当前 watcher（drop 即停止监听）。
#[derive(Default)]
struct WatchState(Mutex<Option<RecommendedWatcher>>);

/// 是否为应忽略的噪声路径：隐藏项（`.` 开头）或大目录（node_modules/target/dist）。
/// 与 list_dir / search_workspace 的过滤保持一致，避免为不可见变更空转。
fn is_noise_path(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.starts_with('.') || s == "node_modules" || s == "target" || s == "dist"
    })
}

/// 把一个事件的有效路径并入本批（跳过纯访问事件与噪声路径）。
fn collect_paths(batch: &mut HashSet<String>, ev: notify::Event) {
    if matches!(ev.kind, EventKind::Access(_)) {
        return; // 打开/读取等访问事件不改变内容，忽略
    }
    for p in ev.paths {
        if is_noise_path(&p) {
            continue;
        }
        batch.insert(p.to_string_lossy().to_string());
    }
}

/// 有变更则把受影响路径列表通过事件发往前端。
fn emit_changes(app: &AppHandle, batch: &HashSet<String>) {
    if batch.is_empty() {
        return;
    }
    let paths: Vec<String> = batch.iter().cloned().collect();
    let _ = app.emit("workspace:fs-change", paths);
}

/// Tauri 命令：监听工作空间目录的文件系统变更（创建/修改/删除/重命名）。
/// 事件经 250ms 去抖后，以受影响路径列表通过 `workspace:fs-change` 发往前端。
/// 再次调用会替换旧监听（切换工作空间时用）。
#[tauri::command]
fn watch_workspace(app: AppHandle, state: State<WatchState>, path: String) -> Result<(), String> {
    let root = Path::new(&path);
    if !root.is_dir() {
        return Err(format!("不是目录: {path}"));
    }
    let (tx, rx) = channel::<notify::Event>();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| format!("创建文件监听失败: {e}"))?;
    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| format!("启动文件监听失败: {e}"))?;

    // 去抖批处理线程：收敛突发事件后成批上报。
    // 当 watcher 被替换/丢弃时，tx 随其闭包销毁 → rx 断开 → 本线程退出。
    std::thread::spawn(move || loop {
        let first = match rx.recv() {
            Ok(ev) => ev,
            Err(_) => break, // 监听已停止
        };
        let mut batch: HashSet<String> = HashSet::new();
        collect_paths(&mut batch, first);
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(ev) => collect_paths(&mut batch, ev),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    emit_changes(&app, &batch);
                    return;
                }
            }
        }
        emit_changes(&app, &batch);
    });

    // 替换旧 watcher（drop 旧值即停止其监听与批处理线程）
    *state.0.lock().unwrap() = Some(watcher);
    Ok(())
}

/// Tauri 命令：判断路径是否存在（外部删除检测用）。
#[tauri::command]
fn path_exists(path: String) -> bool {
    Path::new(&path).exists()
}

// ─────────────────────────── 伪终端（底部终端栏） ───────────────────────────
// 真·交互式终端：portable-pty 起一个系统 shell，输出经后台线程以事件流回前端，
// 前端 xterm.js 渲染；输入 / 尺寸变化再经命令写回。与 VSCode 内置终端体验一致。

/// 一个伪终端会话：master 用于 resize、writer 用于写入用户输入、child 用于关闭时 kill。
struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

/// 终端会话的托管状态：终端 id → 会话（前端可开多个终端；本期 UI 用单个）。
#[derive(Default)]
struct PtyState(Mutex<HashMap<String, PtySession>>);

/// Tauri 命令：打开伪终端，在 `cwd` 起交互 shell；后台线程把输出以 `terminal://data/{id}`
/// 事件（原始字节）发往前端，进程结束发 `terminal://exit/{id}`。
#[tauri::command]
fn terminal_open(
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
fn terminal_write(state: State<PtyState>, id: String, data: String) -> Result<(), String> {
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
fn terminal_resize(state: State<PtyState>, id: String, cols: u16, rows: u16) -> Result<(), String> {
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
fn terminal_close(state: State<PtyState>, id: String) -> Result<(), String> {
    if let Some(mut sess) = state.0.lock().unwrap().remove(&id) {
        let _ = sess.child.kill();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(WatchState::default())
        .manage(PtyState::default())
        .invoke_handler(tauri::generate_handler![
            send_request,
            init_workspace,
            list_dir,
            read_text_file,
            read_file_smart,
            write_text_file,
            create_file,
            create_dir,
            rename_path,
            delete_path,
            search_workspace,
            watch_workspace,
            path_exists,
            terminal_open,
            terminal_write,
            terminal_resize,
            terminal_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 含空格的非法方法应在发起前报错（无需联网）
    #[tokio::test]
    async fn invalid_method_is_rejected() {
        let req = ApiRequest {
            method: "BAD METHOD".into(),
            url: "https://example.com".into(),
            headers: vec![],
            body: None,
        };
        assert!(perform_request(req).await.is_err());
    }

    /// 空 URL 应报错（无需联网）
    #[tokio::test]
    async fn empty_url_is_rejected() {
        let req = ApiRequest {
            method: "GET".into(),
            url: "   ".into(),
            headers: vec![],
            body: None,
        };
        assert!(perform_request(req).await.is_err());
    }

    /// 真实 GET：验证 单节点 DAG 端到端链路（需联网）
    #[tokio::test]
    async fn real_get_request_succeeds() {
        let req = ApiRequest {
            method: "GET".into(),
            url: "https://example.com".into(),
            headers: vec![],
            body: None,
        };
        let resp = perform_request(req).await.expect("请求应成功");
        assert_eq!(resp.status, 200);
        assert!(!resp.body.is_empty());
    }
}
