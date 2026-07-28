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
use tauri::{AppHandle, Emitter, Manager, State};

/// 一对 HTTP 头（请求头 / 响应头通用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub key: String,
    pub value: String,
}

/// multipart/form-data 的一个字段：`file_path` 有值即文件字段（后端读盘发字节），否则是文本字段。
/// `file_name` 与 `content_type` 由前端按路径算好（basename / 扩展名推断），后端不再维护第二份 MIME 表。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

/// 前端传入的 API 请求 —— 即「单节点 DAG」的执行输入。
/// 请求体三选一（优先级 form_data > body_file > body）：
/// multipart 表单 / 二进制文件（原始字节）/ 文本。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    #[serde(default)]
    pub body: Option<String>,
    /// body 类型为 binary 时的文件路径——由后端直接读盘发字节，不经 IPC 搬运大文件
    #[serde(default)]
    pub body_file: Option<String>,
    /// body 类型为 form-data 时的字段列表；Content-Type 与 boundary 交给 reqwest 生成
    #[serde(default)]
    pub form_data: Option<Vec<FormField>>,
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

/// 代理设置（前端「设置 → 代理」）：mode = system | none | custom。
/// system=跟随系统（reqwest 默认读 HTTP(S)_PROXY 环境变量）；none=直连不走代理；custom=指定地址。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    pub mode: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// 工作空间级请求设置（前端「设置 → 通用」，存于 application.yml 的 `settings:`）。
/// 三项均为可选：缺省即「校验开启 / 无自定义 CA / 不限超时」这套安全侧默认。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOptions {
    /// SSL/TLS 证书验证；None 或 true = 校验，false = 接受任何证书（降安全）
    #[serde(default)]
    pub verify_ssl: Option<bool>,
    /// 自定义 CA 证书的**绝对路径**（前端已 join 过工作空间根）
    #[serde(default)]
    pub ca_cert_path: Option<String>,
    /// 整个请求的超时上限（毫秒）；None 或 0 = 不限制
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

const PEM_MARKER: &[u8] = b"-----BEGIN CERTIFICATE-----";

/// 读取 CA 证书文件并解析为 reqwest 证书（PEM 与 DER 两族都支持）。
/// 任何一步失败都**明确报错**——静默忽略会让用户以为配好了却仍连不上，最难排查。
fn load_ca_certificates(path: &str) -> Result<Vec<reqwest::Certificate>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取 CA 证书失败（{path}）: {e}"))?;
    // PEM：文本格式，一个文件里可以串多张（CA 链常打成一个 bundle）
    if bytes.windows(PEM_MARKER.len()).any(|w| w == PEM_MARKER) {
        let certs = reqwest::Certificate::from_pem_bundle(&bytes)
            .map_err(|e| format!("解析 CA 证书失败（{path}）——PEM 格式有误: {e}"))?;
        if certs.is_empty() {
            return Err(format!("解析 CA 证书失败（{path}）——PEM 里没有找到证书"));
        }
        return Ok(certs);
    }
    // DER：二进制，必须是 ASN.1 SEQUENCE（0x30）开头。
    // reqwest 的 from_der 是**惰性**的——真正的解析推迟到 build 客户端时，
    // 连一段纯文本都能构造成功。故先自行挡一道，免得错误延后成一句无指向的「创建客户端失败」。
    if bytes.first() != Some(&0x30) {
        return Err(format!(
            "解析 CA 证书失败（{path}）——既不是 PEM（找不到 BEGIN CERTIFICATE）也不是 DER"
        ));
    }
    reqwest::Certificate::from_der(&bytes)
        .map(|c| vec![c])
        .map_err(|e| format!("解析 CA 证书失败（{path}）——DER 格式有误: {e}"))
}

/// 真正发起请求的逻辑（与 Tauri 解耦，便于测试）。
/// 由后端用 reqwest 发出，天然绕过浏览器 CORS。
async fn perform_request(
    req: ApiRequest,
    proxy: Option<ProxyConfig>,
    options: Option<RequestOptions>,
) -> Result<ApiResponse, String> {
    let url = req.url.trim();
    if url.is_empty() {
        return Err("URL 不能为空".to_string());
    }

    let method = reqwest::Method::from_bytes(req.method.trim().to_uppercase().as_bytes())
        .map_err(|_| format!("非法的 HTTP 方法: {}", req.method))?;

    // 代理：按前端设置决定 —— none 直连、custom 指定地址、其余（system/缺省）用 reqwest 默认（读系统代理环境变量）
    let mut client_builder = reqwest::Client::builder();
    match proxy.as_ref().map(|p| p.mode.as_str()) {
        Some("none") => {
            client_builder = client_builder.no_proxy();
        }
        Some("custom") => {
            let purl = proxy
                .as_ref()
                .and_then(|p| p.url.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match purl {
                Some(u) => {
                    let px = reqwest::Proxy::all(u).map_err(|e| format!("代理地址非法: {e}"))?;
                    client_builder = client_builder.proxy(px);
                }
                None => {
                    client_builder = client_builder.no_proxy(); // custom 但未填地址 → 视为直连
                }
            }
        }
        _ => {}
    }

    // 工作空间设置：证书校验 / 自定义 CA / 超时
    if let Some(opts) = options.as_ref() {
        // 仅显式 false 才关闭校验；此时 reqwest 跳过信任链、有效期与域名匹配（流量仍加密）
        if opts.verify_ssl == Some(false) {
            client_builder = client_builder.danger_accept_invalid_certs(true);
        }
        if let Some(ca) = opts.ca_cert_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            // add_root_certificate 是**追加**到默认信任库，公网 HTTPS 的校验不受影响
            for cert in load_ca_certificates(ca)? {
                client_builder = client_builder.add_root_certificate(cert);
            }
        }
        // 覆盖「连接 + 发送 + 读完响应体」全过程，不是单纯的连接超时
        if let Some(ms) = opts.timeout_ms.filter(|ms| *ms > 0) {
            client_builder = client_builder.timeout(Duration::from_millis(ms));
        }
    }

    let client = client_builder
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut builder = client.request(method, url);
    for h in &req.headers {
        if h.key.trim().is_empty() {
            continue;
        }
        builder = builder.header(h.key.trim(), h.value.as_str());
    }
    // 请求体三选一：multipart 表单 → 二进制文件 → 文本
    let form_fields = req
        .form_data
        .as_ref()
        .map(|f| f.iter().filter(|f| !f.name.trim().is_empty()).collect::<Vec<_>>())
        .filter(|f| !f.is_empty());
    let body_file = req
        .body_file
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(fields) = form_fields {
        // multipart 自带 Content-Type（含 boundary），故放在 header 之后设置，覆盖手填的同名头
        let mut form = reqwest::multipart::Form::new();
        for f in fields {
            let name = f.name.trim().to_string();
            let path = f.file_path.as_deref().map(str::trim).filter(|s| !s.is_empty());
            match path {
                Some(path) => {
                    let bytes = std::fs::read(path)
                        // 多文件表单里只报路径不好定位是哪一行，带上字段名
                        .map_err(|e| format!("读取表单文件失败（{name} → {path}）: {e}"))?;
                    // file_name 前端总会传（basename），兜底防御一手
                    let file_name = f
                        .file_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            Path::new(path)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "file".to_string())
                        });
                    let mut part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
                    if let Some(ct) = f.content_type.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        part = part
                            .mime_str(ct)
                            .map_err(|e| format!("表单文件 Content-Type 非法（{name} → {ct}）: {e}"))?;
                    }
                    form = form.part(name, part);
                }
                None => form = form.text(name, f.value.clone()),
            }
        }
        builder = builder.multipart(form);
    } else if let Some(path) = body_file {
        let bytes = std::fs::read(path).map_err(|e| format!("读取请求体文件失败（{path}）: {e}"))?;
        builder = builder.body(bytes);
    } else if let Some(body) = &req.body {
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
async fn send_request(
    request: ApiRequest,
    proxy: Option<ProxyConfig>,
    options: Option<RequestOptions>,
) -> Result<ApiResponse, String> {
    perform_request(request, proxy, options).await
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

/// 应用设置文件路径：应用配置目录下的 settings.json。
/// 该目录只按应用 identifier 定位（与启动方式无关），跨 dev / 打包一致。
/// macOS: ~/Library/Application Support/com.apicase.app/settings.json
fn app_settings_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("获取应用配置目录失败: {e}"))?;
    Ok(dir.join("settings.json"))
}

/// 应用侧的存储位置（设置页「通用 → 位置」展示用）。
/// `exists` 供前端决定「显示位置」按钮是否可点——该文件在首次写入前并不存在。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPaths {
    pub settings_file: String,
    pub settings_file_exists: bool,
    /// 用户主目录：前端据此把 `/Users/xxx/...` 显示成 `~/...`（省 15+ 字符，多数路径因此能一行放下）。
    /// 仅用于显示，「在文件管理器中显示」仍走原始绝对路径。
    pub home: String,
}

/// Tauri 命令：返回应用自身用到的文件位置。
/// 让用户一眼看清「哪些数据存在哪」——该路径由 Tauri 按 identifier 推导，不同平台各不相同，
/// 不暴露出来的话用户无从知道自己的偏好设置究竟落在哪。
#[tauri::command]
fn app_paths(app: AppHandle) -> Result<AppPaths, String> {
    let file = app_settings_path(&app)?;
    Ok(AppPaths {
        settings_file_exists: file.is_file(),
        settings_file: file.to_string_lossy().into_owned(),
        // 取不到主目录不算错（缩写只是显示优化），退回空串让前端原样显示
        home: app
            .path()
            .home_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    })
}

/// Tauri 命令：读取应用设置（原始 JSON 文本，结构交由前端）。
/// 文件不存在返回空串（前端兜底为默认），其余 IO 错误照常返回 Err。
#[tauri::command]
fn read_app_settings(app: AppHandle) -> Result<String, String> {
    let path = app_settings_path(&app)?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("读取应用设置失败: {e}")),
    }
}

/// Tauri 命令：写入应用设置（整份覆盖）。自动创建配置目录。
#[tauri::command]
fn write_app_settings(app: AppHandle, content: String) -> Result<(), String> {
    let path = app_settings_path(&app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建应用配置目录失败: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("写入应用设置失败: {e}"))
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

/// 可作为 CA 证书使用的扩展名（PEM 与 DER 两族的常见后缀）
const CERT_EXTS: [&str; 5] = ["pem", "crt", "cer", "der", "ca-bundle"];

/// Tauri 命令：递归列出工作空间内的证书文件（设置页「自定义 CA 证书」下拉用）。
/// 返回**相对工作空间根**的路径——存进 application.yml 后随 git 走，换机器/换 clone 目录依然有效。
/// 遍历骨架同 search_workspace：跳过隐藏项与 node_modules/target/dist，结果上限 200。
#[tauri::command]
fn list_cert_files(root: String) -> Result<Vec<String>, String> {
    let root_path = std::path::Path::new(&root);
    if !root_path.is_dir() {
        return Err(format!("不是目录: {root}"));
    }
    const LIMIT: usize = 200;
    let mut out: Vec<String> = Vec::new();
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
            if p.is_dir() {
                if name != "node_modules" && name != "target" && name != "dist" {
                    stack.push(p);
                }
                continue;
            }
            let is_cert = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .map(|e| CERT_EXTS.contains(&e.as_str()))
                .unwrap_or(false);
            if is_cert {
                if let Ok(rel) = p.strip_prefix(root_path) {
                    // 统一用 / 分隔：配置文件要跨平台共享，落盘不能带 Windows 的反斜杠
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                    if out.len() >= LIMIT {
                        out.sort_by_key(|s| s.to_lowercase());
                        return Ok(out);
                    }
                }
            }
        }
    }
    out.sort_by_key(|s| s.to_lowercase());
    Ok(out)
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

/// 递归复制目录内容（隐藏项一并复制——只有 `list_dir` 出于展示需要跳过 `.` 开头）。
fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for ent in std::fs::read_dir(from)? {
        let ent = ent?;
        let src = ent.path();
        let dst = to.join(ent.file_name());
        if ent.file_type()?.is_dir() {
            copy_dir_all(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Tauri 命令：复制路径（文件或目录，用于「克隆」与「复制 / 粘贴」）。
/// 目标唯一名由前端算好；这里仍再校验一次目标已存在与「复制进自己的子目录」（否则会无限递归）。
#[tauri::command]
fn copy_path(from: String, to: String) -> Result<(), String> {
    let src = Path::new(&from);
    let dst = Path::new(&to);
    if !src.exists() {
        return Err(format!("源路径不存在: {from}"));
    }
    if dst.exists() {
        return Err(format!("目标已存在: {to}"));
    }
    if src.is_dir() && dst.starts_with(src) {
        return Err("不能把目录复制到它自己或它的子目录中".to_string());
    }
    if src.is_dir() {
        copy_dir_all(src, dst).map_err(|e| format!("复制目录失败: {e}"))
    } else {
        std::fs::copy(src, dst)
            .map(|_| ())
            .map_err(|e| format!("复制文件失败: {e}"))
    }
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
/// 「关于」页展示的系统信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub os: String,   // 操作系统友好名 + 版本，如 "macOS 14.6"
    pub arch: String, // 架构，如 "arm64" / "x86_64"
    pub chip: String, // 芯片型号（mac 取品牌串，如 "Apple M1 Pro"），其它平台退回架构
}

#[tauri::command]
fn system_info() -> SystemInfo {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
    .to_string();

    #[cfg(target_os = "macos")]
    let os = {
        let ver = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if ver.is_empty() {
            "macOS".to_string()
        } else {
            format!("macOS {}", ver)
        }
    };
    #[cfg(target_os = "windows")]
    let os = "Windows".to_string();
    #[cfg(target_os = "linux")]
    let os = "Linux".to_string();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let os = std::env::consts::OS.to_string();

    // 芯片：macOS 取 CPU 品牌串（如 Apple M1 Pro），其它平台退回架构
    #[cfg(target_os = "macos")]
    let chip = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| arch.clone());
    #[cfg(not(target_os = "macos"))]
    let chip = arch.clone();

    SystemInfo { os, arch, chip }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(WatchState::default())
        .manage(PtyState::default())
        .invoke_handler(tauri::generate_handler![
            send_request,
            init_workspace,
            read_app_settings,
            write_app_settings,
            app_paths,
            list_dir,
            list_cert_files,
            read_text_file,
            read_file_smart,
            write_text_file,
            create_file,
            create_dir,
            rename_path,
            copy_path,
            delete_path,
            search_workspace,
            watch_workspace,
            path_exists,
            terminal_open,
            terminal_write,
            terminal_resize,
            terminal_close,
            system_info
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
            ..Default::default()
        };
        assert!(perform_request(req, None, None).await.is_err());
    }

    /// 空 URL 应报错（无需联网）
    #[tokio::test]
    async fn empty_url_is_rejected() {
        let req = ApiRequest {
            method: "GET".into(),
            url: "   ".into(),
            ..Default::default()
        };
        assert!(perform_request(req, None, None).await.is_err());
    }

    /// binary 请求体指向不存在的文件：应在发起前给出可读错误（无需联网）
    #[tokio::test]
    async fn missing_body_file_is_rejected() {
        let path = std::env::temp_dir().join("apicase-not-exist-body.bin");
        let req = ApiRequest {
            method: "POST".into(),
            url: "https://example.com".into(),
            body_file: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let err = perform_request(req, None, None).await.expect_err("应报错");
        assert!(err.contains("读取请求体文件失败"), "错误信息应指明是请求体文件读取失败: {err}");
    }

    /// form-data 的文件字段指向不存在的文件：错误信息应带上字段名与路径（无需联网）
    #[tokio::test]
    async fn missing_form_file_is_rejected() {
        let path = std::env::temp_dir().join("apicase-not-exist-form.png");
        let req = ApiRequest {
            method: "POST".into(),
            url: "https://example.com".into(),
            form_data: Some(vec![FormField {
                name: "avatar".into(),
                file_path: Some(path.to_string_lossy().into_owned()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let err = perform_request(req, None, None).await.expect_err("应报错");
        assert!(err.contains("读取表单文件失败"), "错误信息应指明是表单文件读取失败: {err}");
        assert!(err.contains("avatar"), "错误信息应带上字段名: {err}");
    }

    /// copy_path：文件复制、目录递归复制、以及两条拒绝规则（无需联网）
    #[test]
    fn copy_path_files_and_dirs() {
        let base = std::env::temp_dir().join("apicase-copy-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src/sub")).expect("建测试目录");
        std::fs::write(base.join("src/a.yml"), b"a").expect("写文件");
        std::fs::write(base.join("src/sub/b.yml"), b"b").expect("写文件");
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();

        // 文件
        copy_path(s(&base.join("src/a.yml")), s(&base.join("a-copy.yml"))).expect("复制文件应成功");
        assert_eq!(std::fs::read(base.join("a-copy.yml")).unwrap(), b"a");

        // 目录（含子目录与其中的文件）
        copy_path(s(&base.join("src")), s(&base.join("dst"))).expect("复制目录应成功");
        assert_eq!(std::fs::read(base.join("dst/sub/b.yml")).unwrap(), b"b");

        // 目标已存在 → 拒绝（不覆盖）
        assert!(copy_path(s(&base.join("src/a.yml")), s(&base.join("a-copy.yml"))).is_err());
        // 目录复制进自己的子目录 → 拒绝（否则无限递归）
        let err = copy_path(s(&base.join("src")), s(&base.join("src/sub/self"))).expect_err("应拒绝");
        assert!(err.contains("自己"), "错误信息应说明原因: {err}");
        // 源不存在 → 拒绝
        assert!(copy_path(s(&base.join("nope")), s(&base.join("x"))).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 测试用自签 CA（`openssl req -x509 -newkey rsa:2048 -days 36500 -subj "/CN=apicase-test-ca"`）。
    /// 只用于验证「能被解析并装进 ClientBuilder」，不参与任何真实握手，故有效期长短无所谓。
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDFzCCAf+gAwIBAgIUOOOO5K92DMLFjeHc18cNQ136Mh8wDQYJKoZIhvcNAQEL\n\
BQAwGjEYMBYGA1UEAwwPYXBpY2FzZS10ZXN0LWNhMCAXDTI2MDcyODA2MzYzOFoY\n\
DzIxMjYwNzA0MDYzNjM4WjAaMRgwFgYDVQQDDA9hcGljYXNlLXRlc3QtY2EwggEi\n\
MA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDRbq2skW/liHTfAbcAPimd/0OC\n\
OZUHkrVLLivzkq2QcMHyvDbZfGVtQwJlWzoMuBCVKuDM1IUJHjegf9ccqy59WyIN\n\
CjNEzNvOXFaWlUw6euL0FLlUnMIKojPbvOsjn2O4FFA+g0yl6eWQk4shAKrHO7d3\n\
+uAtGFR0zPZvkgG8Q0GhDVaoAVHii3YK0x2R2iCUSgwcEai28zMvNAIdvnQojpIB\n\
FodCXN4bgdmHjMeajaexLFBje3k+9BscbCuyprbgponS45cWZo/W0OJSKCSKscG2\n\
PqxI5ZauBpzACgeTp6zD8AHu2vbO8e1oB0OfyWMfQXS+4PSJipajTtbnUlYZAgMB\n\
AAGjUzBRMB0GA1UdDgQWBBQfIMo9w/DlilHgfO7nQbWMpC8zwjAfBgNVHSMEGDAW\n\
gBQfIMo9w/DlilHgfO7nQbWMpC8zwjAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3\n\
DQEBCwUAA4IBAQCZXRIWajEphFSkTCRgfRCDEU6i5ZOBYUamL6htmXT6q3Gw6ZuE\n\
WCCPRPbTPPawOqbcm93QPU+wD8Z4egPR9XQmHkBXdWah9/z8zsA51KqAqHk07m13\n\
kxr6ax3O28kuy7BonYaOm2axKIRKuvGNLFqjCKJzSM1pAyt75oj95GT/wGSoybkr\n\
8vW9QMl0LrAYptQAv9vRUSFqJzhSGlYONfBBlS7WL9tyF4sGn1AwuEZ2yvlwnw/V\n\
jAQHQ5W/qTrZoHrsS9zstP8ENCdKVzlCfq005tKEbPQ74tLVDHe76Sl5lqc6c1iS\n\
DH7Q58rzXuGXGM7MUx1dXEbGRbr7wGIvDp0E\n\
-----END CERTIFICATE-----\n";

    /// CA 证书加载：单张 PEM、PEM bundle（多张串一个文件）、以及三类失败（无需联网）
    #[test]
    fn ca_certificate_loading() {
        let base = std::env::temp_dir().join("apicase-ca-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("建测试目录");
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();

        // 单张 PEM
        let single = base.join("ca.crt");
        std::fs::write(&single, TEST_CA_PEM).expect("写证书");
        assert_eq!(load_ca_certificates(&s(&single)).expect("单张 PEM 应解析成功").len(), 1);

        // bundle：一个文件里串多张（CA 链的常见形态）
        let bundle = base.join("chain.pem");
        std::fs::write(&bundle, format!("{TEST_CA_PEM}{TEST_CA_PEM}")).expect("写证书");
        assert_eq!(load_ca_certificates(&s(&bundle)).expect("bundle 应解析成功").len(), 2);

        // 文件不存在
        let err = load_ca_certificates(&s(&base.join("nope.crt"))).expect_err("应报错");
        assert!(err.contains("读取 CA 证书失败"), "错误应指明是读取阶段失败: {err}");

        // 内容不是证书 → 必须明确报错，不能静默忽略（否则用户以为配好了却仍连不上）
        let junk = base.join("junk.crt");
        std::fs::write(&junk, b"not a certificate at all").expect("写文件");
        let err = load_ca_certificates(&s(&junk)).expect_err("应报错");
        assert!(err.contains("解析 CA 证书失败"), "错误应指明是解析阶段失败: {err}");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 设置装配：坏 CA 路径必须让请求提前失败（而不是丢掉设置照常发出去）（无需联网）
    #[tokio::test]
    async fn bad_ca_path_fails_request() {
        let req = ApiRequest {
            method: "GET".into(),
            url: "https://example.com".into(),
            ..Default::default()
        };
        let opts = RequestOptions {
            ca_cert_path: Some(
                std::env::temp_dir()
                    .join("apicase-absent-ca.crt")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        };
        let err = perform_request(req, None, Some(opts)).await.expect_err("应报错");
        assert!(err.contains("CA 证书"), "错误应指明与 CA 证书有关: {err}");
    }

    /// list_cert_files：按扩展名筛、递归、返回相对路径、跳过隐藏项与大目录（无需联网）
    #[test]
    fn list_cert_files_filters_and_relativizes() {
        let base = std::env::temp_dir().join("apicase-certscan-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("certs")).expect("建目录");
        std::fs::create_dir_all(base.join("node_modules")).expect("建目录");
        std::fs::create_dir_all(base.join(".hidden")).expect("建目录");
        std::fs::write(base.join("root-ca.pem"), b"x").expect("写文件");
        std::fs::write(base.join("certs/ca.crt"), b"x").expect("写文件");
        std::fs::write(base.join("certs/server.CER"), b"x").expect("写文件"); // 大小写不敏感
        std::fs::write(base.join("application.yml"), b"x").expect("写文件"); // 非证书后缀
        std::fs::write(base.join("node_modules/dep.pem"), b"x").expect("写文件"); // 大目录跳过
        std::fs::write(base.join(".hidden/secret.pem"), b"x").expect("写文件"); // 隐藏目录跳过

        let got = list_cert_files(base.to_string_lossy().into_owned()).expect("扫描应成功");
        assert_eq!(got, vec!["certs/ca.crt", "certs/server.CER", "root-ca.pem"]);

        // 非目录应报错
        assert!(list_cert_files(base.join("root-ca.pem").to_string_lossy().into_owned()).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 真实 GET：验证 单节点 DAG 端到端链路（需联网）
    #[tokio::test]
    async fn real_get_request_succeeds() {
        let req = ApiRequest {
            method: "GET".into(),
            url: "https://example.com".into(),
            ..Default::default()
        };
        let resp = perform_request(req, None, None).await.expect("请求应成功");
        assert_eq!(resp.status, 200);
        assert!(!resp.body.is_empty());
    }

    /// 端到端：自签 HTTPS 服务下，三项请求设置各自的真实效果（需先起测试服务，见下）。
    ///
    /// 准备（证书由 TEST_CA_PEM 对应的 CA 签发，SAN 覆盖 localhost 与 127.0.0.1）：
    /// ```sh
    /// openssl req -x509 -newkey rsa:2048 -sha256 -days 36500 -nodes \
    ///   -keyout ca.key -out ca.crt -subj "/CN=apicase-test-ca" \
    ///   -addext "basicConstraints=critical,CA:TRUE"          # 内容需与 TEST_CA_PEM 一致
    /// openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj "/CN=localhost"
    /// openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
    ///   -out server.crt -days 3650 -sha256 \
    ///   -extfile <(printf "subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE")
    /// # 再用 node 起一个 https 服务监听 127.0.0.1:8443，/ 返回 200、/slow 延迟 5s 返回
    /// ```
    /// 手动运行：`cargo test -- --ignored e2e_tls_options`
    ///
    /// 一律走 proxy=none：本机常设 HTTPS_PROXY，不绕开的话根本到不了本地服务。
    #[tokio::test]
    #[ignore]
    async fn e2e_tls_options_against_self_signed_server() {
        let none = || Some(ProxyConfig { mode: "none".into(), url: None });
        let get = |url: &str| ApiRequest {
            method: "GET".into(),
            url: url.into(),
            ..Default::default()
        };

        // ① 默认（校验开启）：自签证书追溯不到受信任根 → 必须失败
        let err = perform_request(get("https://127.0.0.1:8443/"), none(), None)
            .await
            .expect_err("严格校验下自签证书应被拒绝");
        assert!(err.contains("请求失败"), "应是发起阶段的失败: {err}");

        // ② 关闭校验：同一张证书照常连通
        let opts = RequestOptions {
            verify_ssl: Some(false),
            ..Default::default()
        };
        let resp = perform_request(get("https://127.0.0.1:8443/"), none(), Some(opts))
            .await
            .expect("关闭校验后应连通");
        assert_eq!(resp.status, 200);

        // ③ 校验仍开启，但把签发它的 CA 加进信任库 → 同样连通（且公网校验不受影响）
        let ca_path = std::env::temp_dir().join("apicase-e2e-ca.crt");
        std::fs::write(&ca_path, TEST_CA_PEM).expect("写 CA 证书");
        let opts = RequestOptions {
            ca_cert_path: Some(ca_path.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let resp = perform_request(get("https://127.0.0.1:8443/"), none(), Some(opts))
            .await
            .expect("导入 CA 后应连通");
        assert_eq!(resp.status, 200);

        // ④ 超时：/slow 要 5s，给 500ms 必须超时（关校验以排除证书因素）
        let opts = RequestOptions {
            verify_ssl: Some(false),
            timeout_ms: Some(500),
            ..Default::default()
        };
        let start = Instant::now();
        let err = perform_request(get("https://127.0.0.1:8443/slow"), none(), Some(opts))
            .await
            .expect_err("应超时");
        assert!(start.elapsed() < Duration::from_secs(3), "应在超时值附近返回而非等满 5s");
        assert!(err.contains("请求失败"), "超时应体现为请求失败: {err}");

        // ⑤ 超时设为 0 = 不限制：同一个慢接口应能等到结果
        let opts = RequestOptions {
            verify_ssl: Some(false),
            timeout_ms: Some(0),
            ..Default::default()
        };
        let resp = perform_request(get("https://127.0.0.1:8443/slow"), none(), Some(opts))
            .await
            .expect("0 表示不限制，慢接口应等到结果");
        assert_eq!(resp.status, 200);

        let _ = std::fs::remove_file(&ca_path);
    }

    /// 端到端（需本地 httpbin：docker run -d -p 80:80 kennethreitz/httpbin）：
    /// proxy=none 直连本地，覆盖全部 HTTP 方法，验证「不使用代理」能穿透系统代理直达本地服务。
    /// 依赖外部容器，默认 #[ignore]；手动运行：cargo test -- --ignored e2e_methods_via_local_httpbin
    #[tokio::test]
    #[ignore]
    async fn e2e_methods_via_local_httpbin() {
        let none = || Some(ProxyConfig { mode: "none".into(), url: None });
        for (m, url) in [
            ("GET", "http://127.0.0.1/get"),
            ("POST", "http://127.0.0.1/post"),
            ("PUT", "http://127.0.0.1/put"),
            ("PATCH", "http://127.0.0.1/patch"),
            ("DELETE", "http://127.0.0.1/delete"),
            ("HEAD", "http://127.0.0.1/get"),
            ("OPTIONS", "http://127.0.0.1/anything"),
        ] {
            let req = ApiRequest {
                method: m.into(),
                url: url.into(),
                ..Default::default()
            };
            let resp = perform_request(req, none(), None)
                .await
                .unwrap_or_else(|e| panic!("{m} 请求失败: {e}"));
            assert_eq!(resp.status, 200, "{m} 状态应为 200");
        }
    }

    /// 端到端（同上，需本地 httpbin）：multipart 表单与 binary 文件体各发一次，
    /// 由 httpbin 回显校验字段/字节确实送达。
    /// 手动运行：cargo test -- --ignored e2e_body_kinds_via_local_httpbin
    #[tokio::test]
    #[ignore]
    async fn e2e_body_kinds_via_local_httpbin() {
        let none = || Some(ProxyConfig { mode: "none".into(), url: None });

        // 文本字段与文件字段混排，httpbin 把前者回显在 form、后者回显在 files
        let upload = std::env::temp_dir().join("apicase-e2e-upload.txt");
        std::fs::write(&upload, b"apicase-form-file").expect("写临时文件");
        let form = ApiRequest {
            method: "POST".into(),
            url: "http://127.0.0.1/post".into(),
            form_data: Some(vec![
                FormField { name: "a".into(), value: "1".into(), ..Default::default() },
                FormField { name: "b".into(), value: "两".into(), ..Default::default() },
                FormField {
                    name: "doc".into(),
                    file_path: Some(upload.to_string_lossy().into_owned()),
                    file_name: Some("upload.txt".into()),
                    content_type: Some("text/plain".into()),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let resp = perform_request(form, none(), None).await.expect("multipart 请求应成功");
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("\"a\": \"1\""), "httpbin 应回显表单字段 a: {}", resp.body);
        assert!(resp.body.contains("multipart/form-data"), "Content-Type 应由 reqwest 生成: {}", resp.body);
        assert!(resp.body.contains("\"doc\""), "httpbin 应把文件字段回显在 files: {}", resp.body);
        assert!(resp.body.contains("apicase-form-file"), "httpbin 应回显文件内容: {}", resp.body);
        let _ = std::fs::remove_file(&upload);

        let path = std::env::temp_dir().join("apicase-e2e-body.bin");
        std::fs::write(&path, b"apicase-binary-payload").expect("写临时文件");
        let bin = ApiRequest {
            method: "POST".into(),
            url: "http://127.0.0.1/post".into(),
            body_file: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let resp = perform_request(bin, none(), None).await.expect("binary 请求应成功");
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("apicase-binary-payload"), "httpbin 应回显文件字节: {}", resp.body);
        let _ = std::fs::remove_file(&path);
    }
}
