//! 应用级的位置与偏好——**桌面端与 CLI 共用同一份**。
//!
//! # 两层配置，别混
//!
//! - **工作空间级**（`application.yml` 的 `settings:`）：跟着项目走 git，团队共享。
//!   由 `workspace` 模块负责，两端早就是同一份。
//! - **应用级**（本模块，`settings.json`）：这台机器上这个人的偏好，不进版本库。
//!   其中**只有代理会影响执行结果**，其余（主题、快捷键、最近打开）纯属界面的事。
//!
//! 代理此前只有桌面端读得到，于是「界面里设了直连、CLI 照样走系统代理」——
//! 这正好能造出「界面里跑过了、CLI 跑却挂了」，是整套架构最想避免的那种事。
//!
//! # 一律回落，绝不报错
//!
//! 只用界面的人从没跑过 CLI，只用 CLI 的人（CI、容器）根本没有 `settings.json`，
//! 甚至可能连 `HOME` 都没有。**这些都不是错误状态**——取不到就回落默认，
//! 最坏结果是代理跟随系统，而那本来就是默认行为。

use crate::http::ProxyConfig;
use std::path::PathBuf;

/// 应用标识。**必须与 `tauri.conf.json` 的 `identifier` 一致**——
/// 配置目录按它定位，两边对不上就等于各存各的。
pub const APP_IDENTIFIER: &str = "com.apicase.app";

/// 应用设置文件名（桌面端整份读写，本模块只挑自己关心的键读）。
pub const SETTINGS_FILE: &str = "settings.json";

/// 应用配置目录。**语义严格对齐 Tauri 的 `PathResolver::app_config_dir()`**：
///
/// | 平台 | 位置 |
/// |---|---|
/// | macOS | `~/Library/Application Support/<identifier>` |
/// | Linux | `$XDG_CONFIG_HOME/<identifier>`，缺省 `~/.config/<identifier>` |
/// | Windows | `%APPDATA%\<identifier>` |
///
/// 对齐是硬要求：桌面端此前用 Tauri 的实现，两者若给出不同路径，
/// 老用户的设置会表现为「凭空丢了」（其实是读了另一个位置）。
///
/// **不创建目录**，也不检查是否存在——那是写入方的事。取不到主目录返回 `None`。
pub fn app_config_dir() -> Option<PathBuf> {
    let base = config_base()?;
    Some(base.join(APP_IDENTIFIER))
}

/// `settings.json` 的完整路径。
pub fn app_settings_file() -> Option<PathBuf> {
    Some(app_config_dir()?.join(SETTINGS_FILE))
}

#[cfg(target_os = "macos")]
fn config_base() -> Option<PathBuf> {
    home().map(|h| h.join("Library").join("Application Support"))
}

#[cfg(target_os = "windows")]
fn config_base() -> Option<PathBuf> {
    // APPDATA 是漫游配置目录（Tauri 用的也是它，而非 LOCALAPPDATA）
    std::env::var_os("APPDATA").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn config_base() -> Option<PathBuf> {
    // XDG 规范：只认**绝对路径**，相对路径按「未设置」处理
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    xdg.or_else(|| home().map(|h| h.join(".config")))
}

#[allow(dead_code)]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// 应用级偏好里**与执行有关**的部分。
///
/// 刻意只有代理这一项：`settings.json` 里其余的键（主题、快捷键、最近打开、
/// 文件树显隐）都只关乎界面，Rust 侧读它们没有意义，而**每多读一个键就多一处
/// 要与前端 TS 类型同步的地方**。前端仍是这个文件的主人，整份读写；
/// 这里只挑一个键看，加字段互不影响。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppPrefs {
    /// `None` = 没配过，跟随系统（reqwest 默认行为）
    pub proxy: Option<ProxyConfig>,
}

/// 读应用级偏好。**任何一步失败都回落默认**：
/// 没有 `HOME`、目录不存在、文件不存在、JSON 坏了、字段类型不对——一律当作「没配过」。
pub fn load_app_prefs() -> AppPrefs {
    app_settings_file().map(|f| load_prefs_from(&f)).unwrap_or_default()
}

/// 从指定文件读。抽出来是为了**能测而不必去改 `HOME`**——
/// 环境变量是进程全局的，测试并行跑时会互相把对方的设置抹掉
/// （这个坑在定位桌面端时踩过一次，规则是：凡读环境变量的逻辑都要留一个可注入的入口）。
fn load_prefs_from(file: &std::path::Path) -> AppPrefs {
    std::fs::read_to_string(file).map(|t| parse_app_prefs(&t)).unwrap_or_default()
}

/// 从 `settings.json` 文本里挑出执行相关的偏好。独立成函数是为了能测——
/// 读盘那一层没什么可测的，容错规则才是。
pub fn parse_app_prefs(text: &str) -> AppPrefs {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return AppPrefs::default();
    };
    AppPrefs { proxy: v.get("proxy").and_then(parse_proxy) }
}

/// 与前端 `normalizeProxyConfig` 同一套容错：认不出的 `mode` 回落 `system`，
/// `url` 不是字符串就当空。**`system` 视同没配过**——它就是「跟随系统」，
/// 与 `None` 行为一致，返回 `None` 可以让调用方少一层判断。
fn parse_proxy(v: &serde_json::Value) -> Option<ProxyConfig> {
    let mode = match v.get("mode").and_then(serde_json::Value::as_str) {
        Some("none") => "none",
        Some("custom") => "custom",
        _ => return None, // system / 缺失 / 类型不对
    };
    let url = v.get("url").and_then(serde_json::Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    // custom 却没给地址 = 配了一半，按「跟随系统」处理而不是发出一个连不上的请求
    if mode == "custom" && url.is_none() {
        return None;
    }
    Some(ProxyConfig { mode: mode.to_string(), url: url.map(str::to_string) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路径形状：以 identifier 结尾，且落在平台约定的配置根下。
    /// 这条测试挡的是「哪天有人手滑改了 identifier 或拼错了目录层级」——
    /// 那种改动会让所有老用户的设置看起来凭空消失。
    #[test]
    fn config_dir_ends_with_the_identifier() {
        let Some(dir) = app_config_dir() else {
            return; // 没有 HOME 的环境（容器）：取不到不算错
        };
        assert!(dir.ends_with(APP_IDENTIFIER), "{}", dir.display());
        assert_eq!(app_settings_file().unwrap(), dir.join(SETTINGS_FILE));

        #[cfg(target_os = "macos")]
        assert!(
            dir.to_string_lossy().contains("Library/Application Support"),
            "macOS 应落在 Application Support 下：{}",
            dir.display()
        );
    }

    /// 只用界面 / 只用 CLI 的人都不该被另一边绊倒：文件不存在、内容是垃圾、
    /// 键缺失——一律回落「跟随系统」，绝不报错。
    #[test]
    fn anything_unreadable_falls_back_to_defaults() {
        for text in ["", "不是 JSON", "null", "[]", "{}", r#"{"theme":"dark"}"#] {
            assert_eq!(parse_app_prefs(text), AppPrefs::default(), "输入：{text:?}");
        }
        // 文件不存在（只用 CLI 的人从没跑过界面）、目录不存在——都不该 panic
        assert_eq!(load_prefs_from(std::path::Path::new("/绝不存在的目录/settings.json")), AppPrefs::default());
        assert_eq!(load_prefs_from(std::path::Path::new("/")), AppPrefs::default(), "路径是目录也要扛住");
    }

    #[test]
    fn proxy_modes_are_parsed_like_the_frontend() {
        let p = |s: &str| parse_app_prefs(s).proxy;

        assert_eq!(
            p(r#"{"proxy":{"mode":"none","url":""}}"#),
            Some(ProxyConfig { mode: "none".into(), url: None }),
            "直连不需要地址"
        );
        assert_eq!(
            p(r#"{"proxy":{"mode":"custom","url":" http://127.0.0.1:7890 "}}"#),
            Some(ProxyConfig { mode: "custom".into(), url: Some("http://127.0.0.1:7890".into()) }),
            "地址要去掉首尾空白"
        );

        // system 视同没配过——它就是「跟随系统」，与 None 行为一致
        assert_eq!(p(r#"{"proxy":{"mode":"system","url":""}}"#), None);
        // 认不出的 mode 同样回落
        assert_eq!(p(r#"{"proxy":{"mode":"socks5"}}"#), None);
        assert_eq!(p(r#"{"proxy":"字符串不是对象"}"#), None);
        // custom 却没填地址 = 配了一半，按跟随系统处理而不是发一个连不上的请求
        assert_eq!(p(r#"{"proxy":{"mode":"custom","url":"  "}}"#), None);
        assert_eq!(p(r#"{"proxy":{"mode":"custom"}}"#), None);
    }

    /// 前端写出来的完整 settings.json 要能读对，且**不认识的键一概不管**
    #[test]
    fn reads_the_real_frontend_shape_and_ignores_the_rest() {
        let real = r#"{
          "recentWorkspaces": ["/Users/me/demo"],
          "theme": "system",
          "proxy": {"mode": "custom", "url": "http://127.0.0.1:7890"},
          "shortcuts": {},
          "shortcutsEnabled": true,
          "showHiddenFiles": true
        }"#;
        assert_eq!(
            parse_app_prefs(real).proxy,
            Some(ProxyConfig { mode: "custom".into(), url: Some("http://127.0.0.1:7890".into()) })
        );
    }
}
