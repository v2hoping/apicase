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
use std::path::{Path, PathBuf};

/// 应用标识。**必须与 `tauri.conf.json` 的 `identifier` 一致**——
/// 旧版按它定位配置目录，迁移时还要靠它找到老文件。
pub const APP_IDENTIFIER: &str = "com.apicase.app";

/// 应用目录名。主目录下的一个点目录，**三个平台同一个写法**。
pub const APP_DIR_NAME: &str = ".apicase";

/// 应用设置文件名（桌面端整份读写，本模块只挑自己关心的键读）。
pub const SETTINGS_FILE: &str = "settings.json";

/// 覆盖应用目录的环境变量（只认绝对路径）。CI / 容器 / 便携安装要把配置钉在指定位置，
/// 测试也靠它注入——`HOME` 是进程全局的，并行跑会互相把对方的设置抹掉。
pub const HOME_ENV: &str = "APICASE_HOME";

/// 应用配置目录：**`~/.apicase`，三平台一致**（Windows 即 `C:\Users\<用户名>\.apicase`）。
///
/// 此前按各平台的系统约定分三处存（`Library/Application Support` / `XDG_CONFIG_HOME` /
/// `%APPDATA%`），路径又长又各不相同，用户根本记不住自己的配置在哪。apicase 是开发者工具，
/// 配置目录就该像开发者工具（`~/.ssh`、`~/.aws`、`~/.docker`），一句话说得清。
/// 老位置见 [`legacy_app_config_dir`]，仍会被迁移与回落读到。
///
/// `APICASE_HOME` 优先。**不创建目录**，也不检查是否存在——那是写入方的事。
/// 取不到主目录返回 `None`。
pub fn app_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV).map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(dir);
    }
    Some(home()?.join(APP_DIR_NAME))
}

/// `settings.json` 的完整路径。
pub fn app_settings_file() -> Option<PathBuf> {
    Some(app_config_dir()?.join(SETTINGS_FILE))
}

/// 旧的应用配置目录（语义对齐 Tauri 的 `PathResolver::app_config_dir()`）：
///
/// | 平台 | 位置 |
/// |---|---|
/// | macOS | `~/Library/Application Support/<identifier>` |
/// | Linux | `$XDG_CONFIG_HOME/<identifier>`，缺省 `~/.config/<identifier>` |
/// | Windows | `%APPDATA%\<identifier>` |
///
/// 只为老用户留着：迁移的来源，以及迁移发生前的只读回落。
pub fn legacy_app_config_dir() -> Option<PathBuf> {
    Some(legacy_config_base()?.join(APP_IDENTIFIER))
}

/// 旧位置的 `settings.json`。
pub fn legacy_app_settings_file() -> Option<PathBuf> {
    Some(legacy_app_config_dir()?.join(SETTINGS_FILE))
}

#[cfg(target_os = "macos")]
fn legacy_config_base() -> Option<PathBuf> {
    home().map(|h| h.join("Library").join("Application Support"))
}

#[cfg(target_os = "windows")]
fn legacy_config_base() -> Option<PathBuf> {
    // APPDATA 是漫游配置目录（Tauri 用的也是它，而非 LOCALAPPDATA）
    std::env::var_os("APPDATA").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn legacy_config_base() -> Option<PathBuf> {
    // XDG 规范：只认**绝对路径**，相对路径按「未设置」处理
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    xdg.or_else(|| home().map(|h| h.join(".config")))
}

/// 用户主目录。Windows 上**没有 `HOME`**（那是 Unix 的约定），必须认 `USERPROFILE`；
/// 极老的环境里连它都没有，再退到 `HOMEDRIVE` + `HOMEPATH`。
/// 此前只读 `HOME` 没出事，是因为 Windows 走的是 `APPDATA` 分支、根本没用到主目录。
fn home() -> Option<PathBuf> {
    let from = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty());

    #[cfg(target_os = "windows")]
    {
        from("USERPROFILE").or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut s = drive;
            s.push(path);
            Some(PathBuf::from(s)).filter(|p| !p.as_os_str().is_empty())
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        from("HOME")
    }
}

/// 把旧位置的 `settings.json` 搬到 `~/.apicase`。**只在新文件不存在、旧文件存在时动手**，
/// 其余情况（含任何一步失败）一律无声跳过——迁移不成功最坏也就是读到旧位置的值。
///
/// 由**桌面端**调用：它是这个文件唯一的写入方，迁移是写操作。CLI 不做迁移，
/// 它可能跑在只读环境或 CI 里，且桌面端跑过一次后两端自然就统一了。
///
/// 返回是否真的搬了（供调用方打日志/测试断言；调用方通常不必关心）。
pub fn migrate_legacy_settings() -> bool {
    let (Some(new_file), Some(old_file)) = (app_settings_file(), legacy_app_settings_file()) else {
        return false;
    };
    migrate_file(&old_file, &new_file)
}

/// 迁移的实体逻辑。抽出来同样是为了**能测而不必去改环境变量**（见 `load_prefs_from` 的说明）。
fn migrate_file(old_file: &Path, new_file: &Path) -> bool {
    if new_file == old_file || new_file.exists() || !old_file.is_file() {
        return false;
    }
    let Some(new_dir) = new_file.parent() else { return false };
    if std::fs::create_dir_all(new_dir).is_err() {
        return false;
    }
    // rename 最干净（一步到位、不留两份），但跨卷会失败——主目录与 APPDATA 完全可能不在一个盘。
    // 那种情况退成「复制 + 删原文件」；复制成功而删除失败也算搬成功，
    // 新位置已经是权威源，旧文件此后没人再读。
    if std::fs::rename(old_file, new_file).is_err() {
        if std::fs::copy(old_file, new_file).is_err() {
            return false;
        }
        let _ = std::fs::remove_file(old_file);
    }
    // 旧目录里只有这一个文件，搬空了就顺手收拾掉；非空（用户自己放了东西）会失败，无所谓
    if let Some(old_dir) = old_file.parent() {
        let _ = std::fs::remove_dir(old_dir);
    }
    true
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
/// 没有主目录、目录不存在、文件不存在、JSON 坏了、字段类型不对——一律当作「没配过」。
///
/// 新位置读不到就读旧位置：只用 CLI 的人可能从没启动过桌面端，那样迁移永远不会发生，
/// 他在界面里设过的代理不该因为换了个目录就失效。
pub fn load_app_prefs() -> AppPrefs {
    pick_settings_file(app_settings_file(), legacy_app_settings_file())
        .map(|f| load_prefs_from(&f))
        .unwrap_or_default()
}

/// 挑一份要读的：新位置有文件就读新的，否则退回老位置（都没有也返回老位置，
/// 读不到的分支由 `load_prefs_from` 统一回落默认）。抽出来是为了能测。
fn pick_settings_file(new_file: Option<PathBuf>, old_file: Option<PathBuf>) -> Option<PathBuf> {
    new_file.filter(|f| f.is_file()).or(old_file)
}

/// 从指定文件读。抽出来是为了**能测而不必去改 `HOME`**——
/// 环境变量是进程全局的，测试并行跑时会互相把对方的设置抹掉
/// （这个坑在定位桌面端时踩过一次，规则是：凡读环境变量的逻辑都要留一个可注入的入口）。
fn load_prefs_from(file: &Path) -> AppPrefs {
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

    /// 路径形状：**主目录下的 `.apicase`，且三平台同形**。
    /// 这条测试挡的是「哪天有人手滑改了目录名或层级」——
    /// 那种改动会让所有用户的设置看起来凭空消失。
    #[test]
    fn config_dir_is_dot_apicase_under_home() {
        let Some(dir) = app_config_dir() else {
            return; // 没有主目录的环境（容器）：取不到不算错
        };
        // APICASE_HOME 若被外部设了，目录名可以是任何东西，形状断言只在默认路径下成立
        if std::env::var_os(HOME_ENV).is_none() {
            assert!(dir.ends_with(APP_DIR_NAME), "应是 ~/.apicase：{}", dir.display());
            assert_eq!(dir.parent(), home().as_deref(), "应直接挂在主目录下");
            // 老位置要仍能推导出来（迁移与回落都指着它）
            let old = legacy_app_config_dir().expect("老位置应能推导");
            assert!(old.ends_with(APP_IDENTIFIER), "{}", old.display());
            assert_ne!(old, dir, "新老位置不该是同一个");
        }
        assert_eq!(app_settings_file().unwrap(), dir.join(SETTINGS_FILE));
    }

    /// 迁移：新位置没有、旧位置有 → 搬过去且**内容一字不差**，旧文件消失。
    /// 这是老用户升级后设置不丢的唯一保障。
    #[test]
    fn legacy_settings_are_moved_once() {
        let base = std::env::temp_dir().join("apicase-paths-migrate");
        let _ = std::fs::remove_dir_all(&base);
        let old_dir = base.join("老位置").join(APP_IDENTIFIER);
        let new_file = base.join(APP_DIR_NAME).join(SETTINGS_FILE);
        let old_file = old_dir.join(SETTINGS_FILE);
        std::fs::create_dir_all(&old_dir).expect("建目录");
        let content = r#"{"theme":"dark","proxy":{"mode":"none"}}"#;
        std::fs::write(&old_file, content).expect("写文件");

        assert!(migrate_file(&old_file, &new_file), "该搬");
        assert_eq!(std::fs::read_to_string(&new_file).expect("新文件应存在"), content);
        assert!(!old_file.exists(), "旧文件应已搬走");
        assert!(!old_dir.exists(), "搬空的旧目录顺手收拾掉");

        // 幂等：再调一次什么都不做（旧文件已经没了）
        assert!(!migrate_file(&old_file, &new_file));

        // 新位置已有文件时**绝不覆盖**——那是用户在新版里设的，比旧文件新
        std::fs::create_dir_all(&old_dir).expect("建目录");
        std::fs::write(&old_file, "{}").expect("写文件");
        assert!(!migrate_file(&old_file, &new_file), "新文件在就不该动");
        assert_eq!(std::fs::read_to_string(&new_file).expect("读新文件"), content, "内容不该被旧文件覆盖");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 桌面端没跑过（迁移未发生）的机器上，CLI 仍要读得到旧位置的代理设置。
    #[test]
    fn prefs_fall_back_to_the_legacy_file() {
        let base = std::env::temp_dir().join("apicase-paths-fallback");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("建目录");
        let old_file = base.join(SETTINGS_FILE);
        std::fs::write(&old_file, r#"{"proxy":{"mode":"none"}}"#).expect("写文件");

        let missing = base.join("不存在").join(SETTINGS_FILE);
        let pick = |n: PathBuf| pick_settings_file(Some(n), Some(old_file.clone()));
        assert_eq!(pick(missing).as_deref(), Some(old_file.as_path()), "新文件没有 → 读老的");

        // 新文件在就以新的为准（迁移之后的常态）
        let new_file = base.join("新位置.json");
        std::fs::write(&new_file, "{}").expect("写文件");
        assert_eq!(pick(new_file.clone()).as_deref(), Some(new_file.as_path()));

        assert_eq!(load_prefs_from(&old_file).proxy, Some(ProxyConfig { mode: "none".into(), url: None }));

        let _ = std::fs::remove_dir_all(&base);
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
