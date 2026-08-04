//! 工作空间：定位、读配置、组装执行参数、算出报告与 jar 的落点。
//!
//! 一个工作空间 = 一个含 `application.yml` 的目录。这里把「从路径到可以开跑的 `RunOpts`」
//! 这段胶水收成一份——它此前只存在于 `examples/run_workspace.rs` 里，是示例代码而非可复用 API，
//! 而桌面壳、CLI、MCP 三处都要走同一条路。
//!
//! **不含任何执行语义**，只是「配置怎么变成参数」与「文件放在哪」。

use crate::cookie::CookieConfig;
use crate::http::{ClientConfig, ProxyConfig, RequestOptions};
use crate::report::{EnvironmentInfo, WorkspaceInfo};
use crate::runner::RunOpts;
use crate::yaml::{self, Environments};
use crate::WorkspaceSettings;
use std::path::{Path, PathBuf};

/// 工作空间配置文件名——**它的存在与否就是「这是不是一个工作空间」的判据**。
pub const CONFIG_FILE: &str = "application.yml";
/// 工具自己的数据目录（报告、cookie jar）。含明文凭据，故会被写进 `.gitignore`。
pub const DATA_DIR: &str = ".apicase";
/// 报告目录（相对工作空间根）。
pub const REPORTS_DIR: &str = ".apicase/reports";
/// cookie jar（相对工作空间根）。桌面端与 CLI 共用这一个文件，会话因此互通。
pub const COOKIE_JAR: &str = ".apicase/cookies.json";

/// 新建工作空间时写入的 `application.yml` 模板。
pub const CONFIG_TEMPLATE: &str = "# apicase 工作空间配置\n\
# environment：支持多套环境，可切换（dev / test / prod…）\n\
environment:\n  default: {}\n";

/// 一个打开了的工作空间。
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    /// `application.yml` 原文。回写配置要保留其中的未知顶层键，故留着。
    pub config_text: String,
    pub environments: Environments,
    pub settings: WorkspaceSettings,
}

impl Workspace {
    /// 从 `start` 起**向上**查找含 `application.yml` 的目录（同 git 找 `.git`）。
    ///
    /// 这样在工作空间的任何子目录里敲 `apicase run` 都能工作，
    /// `apicase run /别的项目/api/login.yml` 也会自动用那个项目的环境与设置。
    /// 走到文件系统根仍没找到则返回 `None`——**不报错**，"没找到"由调用方决定怎么说。
    pub fn find(start: &Path) -> Option<PathBuf> {
        // 传进来的若是文件，从它所在目录开始找
        let mut dir: &Path = if start.is_file() { start.parent()? } else { start };
        loop {
            if dir.join(CONFIG_FILE).is_file() {
                return Some(dir.to_path_buf());
            }
            dir = dir.parent()?;
        }
    }

    /// 打开一个已知根目录的工作空间。
    ///
    /// **读不到 `application.yml` 不算错**——目录存在就够了，配置回落全默认。
    /// 一个刚 `mkdir` 出来、还没 `init` 的目录仍该能跑用例，而不是先卡在"配置文件呢"。
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        if !root.is_dir() {
            return Err(format!("不是目录：{}", root.display()));
        }
        let config_text = std::fs::read_to_string(root.join(CONFIG_FILE)).unwrap_or_default();
        Ok(Self {
            environments: yaml::parse_environments(&config_text),
            settings: yaml::parse_settings(&config_text),
            config_text,
            root,
        })
    }

    /// `find` + `open`。找不到时的错误里带上起点，否则用户不知道是从哪儿开始找的。
    pub fn discover(start: &Path) -> Result<Self, String> {
        match Self::find(start) {
            Some(root) => Self::open(root),
            None => Err(format!(
                "在 {} 及其上级目录中找不到 {CONFIG_FILE}（可用 apicase init 初始化，或用 -w 指定工作空间）",
                start.display()
            )),
        }
    }

    /// 把一个目录初始化为工作空间：没有 `application.yml` 就写一份模板。**幂等**。
    pub fn init(root: &Path) -> Result<(), String> {
        if !root.is_dir() {
            std::fs::create_dir_all(root).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        let cfg = root.join(CONFIG_FILE);
        if !cfg.exists() {
            std::fs::write(&cfg, CONFIG_TEMPLATE).map_err(|e| format!("写入 {CONFIG_FILE} 失败：{e}"))?;
        }
        Ok(())
    }

    /// 工作空间名 = 根目录名。
    pub fn name(&self) -> String {
        self.root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn info(&self) -> WorkspaceInfo {
        WorkspaceInfo { name: self.name(), root: self.root.to_string_lossy().into_owned() }
    }

    /// 环境名列表，保持 `application.yml` 里的书写顺序（dev/test/prod 的排列是作者的意图）。
    pub fn env_names(&self) -> Vec<String> {
        self.environments.keys().cloned().collect()
    }

    /// 缺省环境 = 配置里的**第一套**；一套都没有时用 `default`
    /// （与 `CONFIG_TEMPLATE` 写的那套同名，新建的工作空间因此开箱就对得上）。
    pub fn default_env(&self) -> String {
        self.environments.keys().next().cloned().unwrap_or_else(|| "default".into())
    }

    /// 取一套环境。`None` 用缺省环境；**名字不存在时不报错**，得到一套空变量表——
    /// 未解析的 `${{var}}` 会原样发出去（内核既有约定），那个现场比"环境名拼错了"更容易看懂。
    /// 名字对不对由调用方按 `env_names()` 自行提示。
    pub fn env_info(&self, name: Option<&str>) -> EnvironmentInfo {
        let name = name.map(str::to_string).unwrap_or_else(|| self.default_env());
        EnvironmentInfo { vars: yaml::env_vars(&self.environments, &name), name }
    }

    /// 工作空间设置 → 客户端配置。
    ///
    /// 两件只有这里知道的事：**CA 的相对路径在此还原为绝对路径**（存盘用相对是为了随 git 走），
    /// **cookie jar 落在 `<root>/.apicase/cookies.json`**（桌面端同一个文件）。
    pub fn client_config(&self, proxy: Option<ProxyConfig>) -> ClientConfig {
        let s = &self.settings;
        ClientConfig {
            proxy,
            options: Some(RequestOptions {
                verify_ssl: (!s.verify_ssl).then_some(false),
                ca_cert_path: (s.use_custom_ca && !s.ca_cert.is_empty())
                    .then(|| self.root.join(&s.ca_cert).to_string_lossy().into_owned()),
                timeout_ms: (s.timeout_ms > 0).then_some(s.timeout_ms),
            }),
            cookies: Some(CookieConfig {
                enabled: s.cookies,
                jar_path: Some(self.jar_path().to_string_lossy().into_owned()),
            }),
        }
    }

    /// 批量运行的默认参数：环境 + 工作空间设置 + 客户端配置。
    /// 并发、`stopOnFailure` 等由调用方在此基础上覆盖。
    pub fn run_opts(&self, env: EnvironmentInfo, proxy: Option<ProxyConfig>) -> RunOpts {
        let mut o = RunOpts::for_batch(env);
        o.continue_on_assertion_failure = self.settings.continue_on_assertion_failure;
        o.client = self.client_config(proxy);
        o
    }

    pub fn reports_dir(&self) -> PathBuf {
        self.root.join(REPORTS_DIR)
    }

    pub fn jar_path(&self) -> PathBuf {
        self.root.join(COOKIE_JAR)
    }

    /// 绝对路径 → **相对工作空间根、`/` 分隔**的路径（报告里记的就是它）。
    /// 落在工作空间之外的路径原样返回——报告要能自证跑的是哪个文件，
    /// 硬拼一串 `../../` 反而更难读。
    pub fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
    }

    /// 把 `.apicase/` 写进工作空间的 `.gitignore`——报告与 cookie jar 是产物且含明文凭据，
    /// 不该进版本库。已有该行就不动；**失败静默**：写不进去不该挡住运行。
    pub fn ensure_gitignore(&self) {
        let gi = self.root.join(".gitignore");
        let text = std::fs::read_to_string(&gi).unwrap_or_default();
        if text.lines().any(|l| matches!(l.trim(), ".apicase/" | ".apicase")) {
            return;
        }
        let next = if text.is_empty() || text.ends_with('\n') {
            format!("{text}.apicase/\n")
        } else {
            format!("{text}\n.apicase/\n")
        };
        let _ = std::fs::write(&gi, next);
    }
}

/// 报告文件名：`<时间戳>-<目标名>.html`。
///
/// **不给它套目录**：报告是自包含单文件（样式脚本数据全内联，就为了能整份转发），
/// 一个只装一个文件的目录只会让每次查看多进一层。
///
/// `stamp` 由调用方给（桌面端与 CLI 都用**本地时间** `YYYYMMDDHHmmss`，见 `util::stamp14`）——
/// core 不引时区库，而这个名字是给人看的，UTC 会让"下午三点跑的"显示成上午七点。
///
/// 目标名的清洗规则与前端 `reportFileName` 逐字对齐（有测试钉住）：去 YAML 后缀 →
/// 文件系统非法字符换 `-` → 合并连续 `-` → 去首尾的 `-` / `.` / 空白 → 截到 60 字节。
/// 截断按**字符边界**切，不会留下半个汉字。
pub fn report_file_name(stamp: &str, target: &str) -> String {
    let base = target.rsplit(['/', '\\']).next().unwrap_or(target);
    let base = strip_yaml_ext(base);

    // 非法字符 → '-'，随后合并连续的 '-'
    let mut cleaned = String::with_capacity(base.len());
    for ch in base.chars() {
        if matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || ch.is_control() {
            if !cleaned.ends_with('-') {
                cleaned.push('-');
            }
        } else {
            cleaned.push(ch);
        }
    }
    let name = clip_bytes(cleaned.trim_matches(|c: char| c == '-' || c == '.' || c.is_whitespace()), 60);

    if name.is_empty() {
        format!("{stamp}.html")
    } else {
        format!("{stamp}-{name}.html")
    }
}

/// 去掉 `.yml` / `.yaml` 后缀（大小写不敏感）。
///
/// 按**字节**比对而不是先切片再比：`&name[len-5..]` 对「用例目录」这种多字节名字会切在
/// 字符中间直接 panic。反过来，字节比对成功恰恰证明末尾那几个字节都是 ASCII，
/// 而 UTF-8 里 ASCII 字节只可能是单字节字符——切点因此必然落在字符边界上。
fn strip_yaml_ext(name: &str) -> &str {
    let b = name.as_bytes();
    for ext in [".yml", ".yaml"] {
        let at = match b.len().checked_sub(ext.len()) {
            Some(at) if at > 0 => at,
            _ => continue,
        };
        if b[at..].eq_ignore_ascii_case(ext.as_bytes()) {
            return &name[..at];
        }
    }
    name
}

/// 按 UTF-8 字节截断，切点落在字符边界上。
fn clip_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("apicase-ws-{name}"));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// 向上查找：子目录里、目标是文件、走到根都找不到
    #[test]
    fn find_walks_up_to_the_config_file() {
        let root = temp("find");
        std::fs::create_dir_all(root.join("api/v2")).expect("建目录");
        std::fs::write(root.join(CONFIG_FILE), CONFIG_TEMPLATE).expect("写配置");
        std::fs::write(root.join("api/v2/login.yml"), "apicase: v0.1\n").expect("写用例");

        assert_eq!(Workspace::find(&root.join("api/v2")).as_deref(), Some(root.as_path()));
        assert_eq!(
            Workspace::find(&root.join("api/v2/login.yml")).as_deref(),
            Some(root.as_path()),
            "目标是文件时从它所在目录开始找"
        );
        // 临时目录本身不该有 application.yml；真有的话这条会失效，故断言在自己造的隔离目录里
        let lonely = temp("find-none");
        std::fs::create_dir_all(&lonely).expect("建目录");
        assert!(
            Workspace::find(&lonely).is_none() || Workspace::find(&lonely).as_deref() != Some(lonely.as_path()),
            "空目录自身不是工作空间"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&lonely);
    }

    /// 没有 application.yml 的目录照样能打开（配置全默认）——刚 mkdir 的目录也该能跑用例
    #[test]
    fn open_falls_back_to_defaults_without_a_config() {
        let root = temp("open");
        std::fs::create_dir_all(&root).expect("建目录");
        let ws = Workspace::open(&root).expect("应能打开");
        assert_eq!(ws.settings, WorkspaceSettings::default());
        assert!(ws.env_names().is_empty());
        assert_eq!(ws.default_env(), "default", "一套环境都没有时用 default");
        assert!(Workspace::open(root.join("不存在")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn config_drives_env_and_client() {
        let root = temp("cfg");
        std::fs::create_dir_all(&root).expect("建目录");
        std::fs::write(
            root.join(CONFIG_FILE),
            "environment:\n  dev: { baseUrl: http://dev }\n  prod: { baseUrl: http://prod }\n\
             settings:\n  verifySsl: false\n  useCustomCa: true\n  caCert: certs/ca.pem\n  timeoutMs: 3000\n",
        )
        .expect("写配置");
        let ws = Workspace::open(&root).expect("应能打开");

        assert_eq!(ws.env_names(), vec!["dev", "prod"], "保持书写顺序");
        assert_eq!(ws.default_env(), "dev", "缺省 = 第一套");
        assert_eq!(ws.env_info(None).vars.get("baseUrl").map(String::as_str), Some("http://dev"));
        assert_eq!(ws.env_info(Some("prod")).vars.get("baseUrl").map(String::as_str), Some("http://prod"));
        assert!(ws.env_info(Some("没这套")).vars.is_empty(), "环境名不存在得到空表，不报错");

        let cfg = ws.client_config(None);
        let opts = cfg.options.expect("应有请求选项");
        assert_eq!(opts.verify_ssl, Some(false));
        assert_eq!(opts.timeout_ms, Some(3000));
        assert_eq!(
            opts.ca_cert_path.as_deref(),
            Some(root.join("certs/ca.pem").to_string_lossy().as_ref()),
            "CA 的相对路径要还原成绝对路径"
        );
        let jar = cfg.cookies.expect("应有 cookie 配置");
        assert!(jar.enabled);
        assert_eq!(jar.jar_path.as_deref(), Some(root.join(COOKIE_JAR).to_string_lossy().as_ref()));

        // 失败传播策略跟工作空间配置走
        assert!(!ws.run_opts(ws.env_info(None), None).continue_on_assertion_failure);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn init_is_idempotent_and_gitignore_is_written_once() {
        let root = temp("init");
        Workspace::init(&root).expect("初始化");
        std::fs::write(root.join(CONFIG_FILE), "environment:\n  dev: {}\n").expect("改配置");
        Workspace::init(&root).expect("再次初始化");
        assert_eq!(
            std::fs::read_to_string(root.join(CONFIG_FILE)).expect("读配置"),
            "environment:\n  dev: {}\n",
            "已有配置不该被模板覆盖"
        );

        let ws = Workspace::open(&root).expect("应能打开");
        ws.ensure_gitignore();
        assert_eq!(std::fs::read_to_string(root.join(".gitignore")).expect("读"), ".apicase/\n");
        ws.ensure_gitignore();
        assert_eq!(std::fs::read_to_string(root.join(".gitignore")).expect("读"), ".apicase/\n", "不重复追加");

        // 已有内容且没有末尾换行时要补一个
        std::fs::write(root.join(".gitignore"), "dist").expect("写");
        ws.ensure_gitignore();
        assert_eq!(std::fs::read_to_string(root.join(".gitignore")).expect("读"), "dist\n.apicase/\n");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rel_is_relative_and_slash_separated() {
        let root = temp("rel");
        std::fs::create_dir_all(&root).expect("建目录");
        let ws = Workspace::open(&root).expect("应能打开");
        assert_eq!(ws.rel(&root.join("api").join("login.yml")), "api/login.yml");
        assert_eq!(ws.rel(Path::new("/somewhere/else.yml")), "/somewhere/else.yml", "空间外的原样返回");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 与前端 `reportFileName` 逐字对齐——两处生成的名字不同，报告目录里就会出现两套命名
    #[test]
    fn report_file_name_matches_the_frontend() {
        let s = "20260803143022";
        assert_eq!(report_file_name(s, "api/login.yml"), "20260803143022-login.html");
        assert_eq!(report_file_name(s, "smoke.YAML"), "20260803143022-smoke.html", "后缀大小写不敏感");
        assert_eq!(report_file_name(s, "api"), "20260803143022-api.html", "目录名照用");
        assert_eq!(report_file_name(s, "a:b*c?.yml"), "20260803143022-a-b-c.html", "非法字符换 -，连续的合并");
        assert_eq!(report_file_name(s, "...yml"), "20260803143022.html", "清完是空的就只留时间戳");
        assert_eq!(report_file_name(s, "用例目录"), "20260803143022-用例目录.html");
        // 60 字节截断，切点落在字符边界（每个汉字 3 字节 → 20 个）
        let long = "长".repeat(30);
        assert_eq!(report_file_name(s, &long), format!("{s}-{}.html", "长".repeat(20)));
    }
}
