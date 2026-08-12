//! 应用级命令：工作空间初始化、应用设置文件、证书扫描、系统信息。
//!
//! 「应用设置」与「工作空间设置」是两回事，别混：前者是这台机器上这个人的偏好
//! （主题、快捷键、最近打开），存应用配置目录、不进版本库；后者跟着项目走 git
//! （`application.yml` 的 `settings:`），团队共享——那部分归 core 解析。

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Tauri 命令：把一个目录初始化为 apicase 工作空间。
/// 工作空间根需有 `application.yml`（工作空间配置文件）；若不存在则写入一份初始模板。
///
/// 转发给 core —— 模板内容与「什么算工作空间」的判据只该有一份，`apicase init` 走的是同一个函数。
#[tauri::command]
pub fn init_workspace(path: String) -> Result<(), String> {
    let dir = std::path::Path::new(&path);
    if !dir.is_dir() {
        return Err(format!("目录不存在: {path}"));
    }
    apicase_core::workspace::Workspace::init(dir)
}

/// Tauri 命令：启动参数里指定的工作空间（`Apicase /path/to/ws`）。
///
/// 这是 **`apicase gui <路径>` 落地的一半**：CLI 找到桌面端后把工作空间作为参数传进来，
/// 界面起来就直接进那个目录，而不是停在「请选择工作空间」。顺带也让
/// 「把目录拖到应用图标上」这类系统集成有了着落。
///
/// **只认「存在且是目录」的参数**，其余一律忽略：macOS 从 Finder 启动会塞一个
/// `-psn_0_123456`（进程序列号），Windows / Linux 的桌面项也可能带自己的开关。
/// 逐个筛而不是取 `argv[1]`，这些参数出现的位置并不固定。
#[tauri::command]
pub fn startup_workspace() -> Option<String> {
    pick_workspace_arg(std::env::args().skip(1))
}

/// argv → 工作空间路径。抽成纯函数是为了能测：读 `std::env::args()` 的版本在单测里没法喂参数，
/// 而「哪些参数该跳过」正是这里唯一容易写错的地方。
fn pick_workspace_arg(args: impl Iterator<Item = String>) -> Option<String> {
    args.map(std::path::PathBuf::from)
        .find(|p| p.is_dir())
        // 规范化：CLI 可能传的是相对路径，而界面里一切都按绝对路径走
        .map(|p| p.canonicalize().unwrap_or(p).to_string_lossy().into_owned())
}

/// 应用设置文件路径：`~/.apicase/settings.json`，**三平台同一个写法**
/// （Windows 即 `C:\Users\<用户名>\.apicase\settings.json`）。与启动方式无关，跨 dev / 打包一致。
///
/// **走 core 的实现而不是 Tauri 的 `PathResolver`**：一来 CLI 也要读这个文件里的代理设置
/// （否则「界面里设了直连、CLI 照样走系统代理」），而 CLI 里没有 Tauri，两份路径推导必然漂移，
/// 漂移的表现是「用户的设置凭空丢了」；二来 `PathResolver` 给的是各平台的系统约定目录
/// （`Library/Application Support` / `%APPDATA%` / XDG），三条路径又长又各不相同——
/// 而这正是要摆脱的东西。
fn app_settings_path(_app: &AppHandle) -> Result<std::path::PathBuf, String> {
    apicase_core::paths::app_settings_file().ok_or_else(|| "获取应用配置目录失败".to_string())
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
pub fn app_paths(app: AppHandle) -> Result<AppPaths, String> {
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
pub fn read_app_settings(app: AppHandle) -> Result<String, String> {
    let path = app_settings_path(&app)?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("读取应用设置失败: {e}")),
    }
}

/// Tauri 命令：写入应用设置（整份覆盖）。自动创建配置目录。
#[tauri::command]
pub fn write_app_settings(app: AppHandle, content: String) -> Result<(), String> {
    let path = app_settings_path(&app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建应用配置目录失败: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("写入应用设置失败: {e}"))
}

/// 可作为 CA 证书使用的扩展名（PEM 与 DER 两族的常见后缀）
const CERT_EXTS: [&str; 5] = ["pem", "crt", "cer", "der", "ca-bundle"];

/// Tauri 命令：递归列出工作空间内的证书文件（设置页「自定义 CA 证书」下拉用）。
/// 返回**相对工作空间根**的路径——存进 application.yml 后随 git 走，换机器/换 clone 目录依然有效。
/// 遍历骨架同 search_workspace：跳过隐藏项与 node_modules/target/dist，结果上限 200。
#[tauri::command]
pub fn list_cert_files(root: String) -> Result<Vec<String>, String> {
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

/// 「关于」页展示的系统信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub os: String,   // 操作系统友好名 + 版本，如 "macOS 14.6"
    pub arch: String, // 架构，如 "arm64" / "x86_64"
    pub chip: String, // 芯片型号（mac 取品牌串，如 "Apple M1 Pro"），其它平台退回架构
}

#[tauri::command]
pub fn system_info() -> SystemInfo {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 设置文件落在**主目录下的 `.apicase`**，而**老位置必须与 Tauri 的 `PathResolver`
    /// 逐字一致**——桌面端此前用的就是它，迁移要从那儿把老用户的文件搬过来，
    /// 推导对不上就等于没搬（用户看到「设置凭空丢了」，且不会有任何报错）。
    ///
    /// 顺带挡住「哪天有人图省事把 `app_settings_path` 改回 `PathResolver`」：
    /// 统一位置的意义正在于三平台一句话说得清，退回去就白改了。
    ///
    /// `app_settings_path` 忽略传入的 handle（只转发 core），故这里直接比 core 的结果——
    /// mock app 的 runtime 与命令签名里的 `AppHandle<Wry>` 也对不上。
    #[test]
    fn settings_live_in_dot_apicase_and_legacy_matches_tauri() {
        if std::env::var_os(apicase_core::paths::HOME_ENV).is_some() {
            return; // 外部把目录钉到了别处（CI / 容器），形状断言只在默认路径下成立
        }
        let app = tauri::test::mock_app();
        let tauri_base = app.path().config_dir().expect("Tauri 应能给出配置根");
        let path = apicase_core::paths::app_settings_file().expect("应能给出设置文件路径");

        assert!(path.ends_with(".apicase/settings.json"), "应是 ~/.apicase/settings.json：{}", path.display());
        assert!(!path.starts_with(&tauri_base), "不该再落在平台配置根下：{}", path.display());

        // 迁移的来源：平台配置根 / identifier / settings.json
        let legacy = apicase_core::paths::legacy_app_settings_file().expect("老位置应能推导");
        assert_eq!(
            legacy,
            tauri_base.join(apicase_core::paths::APP_IDENTIFIER).join("settings.json"),
            "老位置要与 Tauri 的推导逐字一致，否则老用户的文件根本搬不过来"
        );
    }

    /// 启动参数里只认「存在且是目录」的那一个：macOS 从 Finder 启动会塞 `-psn_0_123456`，
    /// 桌面项也可能带自己的开关。取 argv[1] 就会把这些当成工作空间。
    #[test]
    fn startup_arg_picks_the_only_real_directory() {
        let base = std::env::temp_dir().join("apicase-startup-arg");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("建目录");
        std::fs::write(base.join("a.yml"), b"x").expect("写文件");
        let dir = base.to_string_lossy().into_owned();
        let want = base.canonicalize().unwrap_or(base.clone()).to_string_lossy().into_owned();

        let got = pick_workspace_arg(
            ["-psn_0_123456".into(), "--flag".into(), "/绝不存在的目录".into(), dir.clone()].into_iter(),
        );
        assert_eq!(got.as_deref(), Some(want.as_str()), "要跳过开关与不存在的路径");

        // 文件不是工作空间
        assert!(pick_workspace_arg([base.join("a.yml").to_string_lossy().into_owned()].into_iter()).is_none());
        // 什么都没给 = 停在「请选择工作空间」，不是错
        assert!(pick_workspace_arg(std::iter::empty()).is_none());

        let _ = std::fs::remove_dir_all(&base);
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
}
