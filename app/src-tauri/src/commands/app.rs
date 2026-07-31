//! 应用级命令：工作空间初始化、应用设置文件、证书扫描、系统信息。
//!
//! 「应用设置」与「工作空间设置」是两回事，别混：前者是这台机器上这个人的偏好
//! （主题、快捷键、最近打开），存应用配置目录、不进版本库；后者跟着项目走 git
//! （`application.yml` 的 `settings:`），团队共享——那部分归 core 解析。

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Tauri 命令：把一个目录初始化为 apicase 工作空间。
/// 工作空间根需有 `application.yml`（工作空间配置文件）；若不存在则写入一份初始模板。
#[tauri::command]
pub fn init_workspace(path: String) -> Result<(), String> {
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
