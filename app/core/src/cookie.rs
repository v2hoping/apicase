//! Cookie jar：自动收 `Set-Cookie`、按域回带、按工作空间持久化。
//!
//! # 为什么不自己解析
//!
//! RFC 6265 的域匹配（`Domain=.example.com` 覆盖子域）、路径匹配、`Secure` / `HttpOnly`
//! 的适用条件、`Expires` 与 `Max-Age` 的优先级……这套规则自己写的每一种错法，
//! 表现都是「某个 cookie 莫名其妙没带上」——最难排查的那类问题。故用 `cookie_store`
//! （reqwest 自己的 `Jar` 用的也是它），我们只负责持久化、开关与管理接口。
//!
//! # 为什么要装进 reqwest 而不是在响应回来后自己收
//!
//! **重定向链**。登录接口的典型形态是 `POST /login → 302 + Set-Cookie → GET /home`，
//! 中间那一跳的响应我们根本看不到（reqwest 内部就跟完了）；而重定向后的那一跳
//! 又需要带上刚拿到的 cookie。把 jar 实现成 `reqwest::cookie::CookieStore` 装进客户端，
//! 这两件事就都在链路内部完成了。
//!
//! 发送前仍会**自己算一次 Cookie 头写进请求**（`attach`），为的是让报告如实记录本次带了什么——
//! reqwest 只在请求没有 `Cookie` 头时才由 jar 补，两条路径不会打架。

use crate::http::ClientConfig;
use crate::request::HttpRequest;
use crate::report::KvPair;
use crate::util::http_date;
use cookie_store::{CookieExpiration, CookieStore, RawCookie};
use reqwest::header::HeaderValue;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

/// 落盘节流间隔。批量跑 200 个用例不该产生 200 次写盘；关掉应用又不该丢会话，
/// 故变更只标脏，最快 1s 落一次，收尾时由 `flush_all` 强制写一次。
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// 一次执行的 cookie 配置（由上层下发）。
///
/// **缺省是不启用**：core 不猜工作空间在哪，没人告诉它 jar 该放哪儿就不自作主张。
/// 「默认开」是工作空间设置那一层的默认（`settings.cookies`），由前端显式下发。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieConfig {
    /// 自动收发的总开关
    #[serde(default)]
    pub enabled: bool,
    /// jar 的落盘路径（绝对路径；桌面端是 `<workspace>/.apicase/cookies.json`）。
    /// 缺省 = 只在内存里活着（CLI 的一次性执行、尚未打开工作空间时）
    #[serde(default)]
    pub jar_path: Option<String>,
}

impl CookieConfig {
    /// 进程内区分不同 jar 的键（也参与 HTTP 客户端的缓存键）。
    fn key(&self) -> String {
        self.jar_path.as_deref().unwrap_or_default().trim().to_string()
    }
}

/// 管理界面看到的一条 cookie。`domain + path + name` 是 cookie 的主键——
/// 只给 name 会误删同名不同域的那条。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieItem {
    pub domain: String,
    pub path: String,
    pub name: String,
    pub value: String,
    pub secure: bool,
    pub http_only: bool,
    /// 过期时间（Unix 毫秒）；None = 会话 cookie。
    /// 给毫秒而不是格式化好的串：编辑框要拿它回填，显示格式是渲染层的事。
    pub expires_ms: Option<u64>,
    /// 已过期：不会再被发送，但仍列出来——否则用户看到「没有 cookie」却又删不掉它
    pub expired: bool,
    /// 无 `Domain` 属性 = 只发给这一个主机。**store 里两者的 domain 字符串长得一样**
    /// （`Suffix` 存的也是去点后的域名），不单独标出来，编辑一次就把子域通配悄悄丢了。
    pub host_only: bool,
}

/// 管理界面提交的一条 cookie（新增或修改）。
///
/// 与 `CookieItem` 分开：那个是"读出来的样子"（带 `expired` 这类算出来的字段），
/// 这个是"要写进去的东西"，字段少一半，也不该让调用方去填算出来的值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieInput {
    /// 以 `.` 开头 = 带 `Domain` 属性（子域一并生效），否则只发给这一个主机
    pub domain: String,
    #[serde(default)]
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub expires_ms: Option<u64>,
}

/// cookie 的主键（改名 / 换域时用来删掉原来那条）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieKey {
    pub domain: String,
    pub path: String,
    pub name: String,
}

// ── jar ─────────────────────────────────────────────

#[derive(Debug, Default)]
struct WriteState {
    dirty: bool,
    last: Option<Instant>,
}

/// 一份 cookie jar：内存 store + 可选的落盘位置。
#[derive(Debug)]
pub struct CookieJar {
    path: Option<PathBuf>,
    store: RwLock<CookieStore>,
    write: Mutex<WriteState>,
}

impl CookieJar {
    /// 从磁盘读一份（文件不存在 / 读坏了都回落空 jar——一份读不回来的 cookie 文件
    /// 不该让请求发不出去，最坏结果只是重新登录一次）。
    fn load(path: Option<PathBuf>) -> Self {
        let store = path
            .as_deref()
            .and_then(|p| std::fs::File::open(p).ok())
            // load_all 而非 load：过期的也读回来，管理界面才列得出、删得掉（gc 在落盘时做）
            .and_then(|f| cookie_store::serde::json::load_all(std::io::BufReader::new(f)).ok())
            .unwrap_or_default();
        Self { path, store: RwLock::new(store), write: Mutex::new(WriteState::default()) }
    }

    /// 锁中毒不让 jar 报废（同 `ClientPool::lock` 的理由）：里面只是可重建的 cookie 数据，
    /// 照直 unwrap 会让一次无关的 panic 把此后每个请求都打死。
    fn read(&self) -> RwLockReadGuard<'_, CookieStore> {
        self.store.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write_store(&self) -> RwLockWriteGuard<'_, CookieStore> {
        self.store.write().unwrap_or_else(|e| e.into_inner())
    }

    /// 该 URL 匹配到的 `Cookie` 头值（`a=1; b=2`）；没有匹配项返回 None。
    pub fn header_for(&self, url: &str) -> Option<String> {
        let u = Url::parse(url.trim()).ok()?;
        let s = self
            .read()
            .get_request_values(&u)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        (!s.is_empty()).then_some(s)
    }

    /// 全部 cookie（含会话与已过期的），按 域 → 路径 → 名 排序。
    pub fn list(&self) -> Vec<CookieItem> {
        let mut out: Vec<CookieItem> = self
            .read()
            .iter_any()
            .map(|c| CookieItem {
                domain: String::from(&c.domain),
                path: String::from(&c.path),
                name: c.name().to_string(),
                value: c.value().to_string(),
                secure: c.secure().unwrap_or(false),
                http_only: c.http_only().unwrap_or(false),
                expires_ms: match &c.expires {
                    // 1970 前的时间戳只可能来自坏数据，按「无过期时间」处理而不是算出负数
                    CookieExpiration::AtUtc(t) => {
                        u64::try_from(t.unix_timestamp()).ok().map(|s| s.saturating_mul(1000))
                    }
                    CookieExpiration::SessionEnd => None,
                },
                expired: c.is_expired(),
                host_only: matches!(c.domain, cookie_store::CookieDomain::HostOnly(_)),
            })
            .collect();
        out.sort_by(|a, b| (&a.domain, &a.path, &a.name).cmp(&(&b.domain, &b.path, &b.name)));
        out
    }

    /// 新增或修改一条 cookie（管理界面的「＋」与铅笔走同一个入口）。
    ///
    /// `prev` 是修改前的主键：改了域 / 路径 / 名等于换了一条 cookie，不删旧的就会留下一条孤儿。
    ///
    /// 实现走「拼一行 `Set-Cookie` 交给解析器」而不是自己拼装属性对象：
    /// 这样域、路径、过期的合法性判定与真实响应走的是同一套代码，不会出现
    /// 「手工加的能用、服务端下发的不能用」这种两套语义的分裂。
    pub fn put(&self, prev: Option<&CookieKey>, item: &CookieInput) -> Result<(), String> {
        let name = item.name.trim();
        if name.is_empty() {
            return Err("Cookie 名不能为空".into());
        }
        let domain = item.domain.trim();
        let host = domain.trim_start_matches('.');
        if host.is_empty() {
            return Err("域不能为空".into());
        }
        let path = match item.path.trim() {
            "" => "/",
            p => p,
        };
        if !path.starts_with('/') {
            return Err("路径要以 / 开头".into());
        }
        // Secure 的 cookie 只有 https 页面才存得下，故用它决定这个"虚拟请求地址"的协议
        let scheme = if item.secure { "https" } else { "http" };
        let url = Url::parse(&format!("{scheme}://{host}{path}")).map_err(|e| format!("域不合法（{domain}）: {e}"))?;

        let mut line = format!("{name}={}; Path={path}", item.value);
        // 带前导点 = 用户要子域一并生效，翻译成 Domain 属性；不带则不写该属性（host-only）
        if domain.starts_with('.') {
            line.push_str(&format!("; Domain={domain}"));
        }
        if item.secure {
            line.push_str("; Secure");
        }
        if item.http_only {
            line.push_str("; HttpOnly");
        }
        if let Some(ms) = item.expires_ms {
            line.push_str(&format!("; Expires={}", http_date(ms)));
        }
        let raw = RawCookie::parse(line).map_err(|e| format!("Cookie 不合法: {e}"))?;

        {
            let mut st = self.write_store();
            if let Some(p) = prev {
                st.remove(&p.domain, &p.path, &p.name);
            }
            st.insert_raw(&raw, &url).map_err(|e| match e {
                // 这两个是用户填错时最常撞上的，原文是英文且过于技术化，换成能直接照做的说法
                cookie_store::CookieError::Expired => "过期时间已经过去了，这条 cookie 存不下".to_string(),
                cookie_store::CookieError::DomainMismatch => {
                    format!("域 {domain} 与该 cookie 不匹配")
                }
                other => format!("保存失败: {other}"),
            })?;
        }
        self.mark_dirty();
        self.flush();
        Ok(())
    }

    /// 删一条，返回是否真的删掉了。
    pub fn remove(&self, domain: &str, path: &str, name: &str) -> bool {
        let removed = self.write_store().remove(domain, path, name).is_some();
        if removed {
            self.mark_dirty();
            self.flush();
        }
        removed
    }

    /// 清空：`domain` 给了就只清该域，否则全清。返回清掉的条数。
    pub fn clear(&self, domain: Option<&str>) -> usize {
        let n = {
            let mut st = self.write_store();
            match domain {
                None => {
                    let n = st.iter_any().count();
                    st.clear();
                    n
                }
                Some(d) => {
                    let keys: Vec<(String, String, String)> = st
                        .iter_any()
                        .filter(|c| String::from(&c.domain) == d)
                        .map(|c| (String::from(&c.domain), String::from(&c.path), c.name().to_string()))
                        .collect();
                    let n = keys.len();
                    for (dm, p, nm) in keys {
                        st.remove(&dm, &p, &nm);
                    }
                    n
                }
            }
        };
        if n > 0 {
            self.mark_dirty();
            self.flush();
        }
        n
    }

    fn mark_dirty(&self) {
        self.write.lock().unwrap_or_else(|e| e.into_inner()).dirty = true;
    }

    /// 有变更就落盘（节流由调用方决定：`flush` 强制，`flush_throttled` 最快 1s 一次）。
    pub fn flush(&self) {
        self.save_if_dirty(true);
    }

    fn flush_throttled(&self) {
        self.save_if_dirty(false);
    }

    fn save_if_dirty(&self, force: bool) {
        let Some(path) = self.path.clone() else { return };
        {
            let mut w = self.write.lock().unwrap_or_else(|e| e.into_inner());
            if !w.dirty {
                return;
            }
            if !force && w.last.is_some_and(|t| t.elapsed() < FLUSH_INTERVAL) {
                return;
            }
            // 先清标记再写：写盘期间来的新 cookie 会重新标脏，下一轮再写，
            // 不会因为「写完才清」而把这期间的变更一并吞掉
            w.dirty = false;
            w.last = Some(Instant::now());
        }
        // gc 掉已过期的，否则文件会一直涨——过期 cookie 既不发送也没有保留价值
        self.gc();
        let mut buf: Vec<u8> = Vec::new();
        // 保存**含会话 cookie**：浏览器的语义是"关标签页就丢"，但 apicase 的使用形态是
        // 「昨天登录过、今天接着调」，这里对齐 Postman 而不是对齐浏览器
        if cookie_store::serde::json::save_incl_expired_and_nonpersistent(&self.read(), &mut buf).is_err() {
            return;
        }
        write_atomic(&path, &buf);
    }

    fn gc(&self) {
        let mut st = self.write_store();
        let expired: Vec<(String, String, String)> = st
            .iter_any()
            .filter(|c| c.is_expired())
            .map(|c| (String::from(&c.domain), String::from(&c.path), c.name().to_string()))
            .collect();
        for (d, p, n) in expired {
            st.remove(&d, &p, &n);
        }
    }
}

/// 写盘走「临时文件 + rename」：直接覆写时若进程在半途没了，留下的是半截 JSON，
/// 下次加载失败即整份会话丢失。失败一律静默——cookie 落盘不该让请求出错。
fn write_atomic(path: &Path, data: &[u8]) {
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, data).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

// ── reqwest 集成 ────────────────────────────────────

impl reqwest::cookie::CookieStore for CookieJar {
    fn set_cookies(&self, headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let cookies: Vec<RawCookie<'static>> = headers
            .filter_map(|v| std::str::from_utf8(v.as_bytes()).ok())
            .filter_map(|s| RawCookie::parse(s.to_string()).ok())
            .collect();
        if cookies.is_empty() {
            return;
        }
        self.write_store().store_response_cookies(cookies.into_iter(), url);
        self.mark_dirty();
        self.flush_throttled();
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        HeaderValue::from_str(&self.header_for(url.as_str())?).ok()
    }
}

// ── 进程内注册表 ────────────────────────────────────

fn jars() -> &'static Mutex<HashMap<String, Arc<CookieJar>>> {
    static JARS: OnceLock<Mutex<HashMap<String, Arc<CookieJar>>>> = OnceLock::new();
    JARS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn registry() -> std::sync::MutexGuard<'static, HashMap<String, Arc<CookieJar>>> {
    jars().lock().unwrap_or_else(|e| e.into_inner())
}

/// 取（或建）某个落盘路径对应的 jar。同一路径全进程共用一份内存 store，
/// 否则两个客户端各自持一份，谁后写盘谁把对方的会话抹掉。
///
/// `path` 为空 = 只在内存里活着的那一份。
pub fn jar_at(path: Option<&str>) -> Arc<CookieJar> {
    let key = path.unwrap_or_default().trim().to_string();
    let mut reg = registry();
    if let Some(j) = reg.get(&key) {
        return j.clone();
    }
    let jar = Arc::new(CookieJar::load((!key.is_empty()).then(|| PathBuf::from(&key))));
    reg.entry(key).or_insert(jar).clone()
}

/// 该配置要用的 jar；开关关着就是 None（不带、不收，但已存的文件原样保留）。
pub fn jar_for(cfg: &ClientConfig) -> Option<Arc<CookieJar>> {
    let c = cfg.cookies.as_ref().filter(|c| c.enabled)?;
    Some(jar_at(Some(&c.key())))
}

/// 参与 HTTP 客户端缓存键的那一段：cookie provider 是 builder 级设置，
/// 不进缓存键就会出现「关了开关仍在带 cookie」这类跟着缓存走的幽灵行为。
pub fn client_key(cfg: &ClientConfig) -> String {
    match cfg.cookies.as_ref().filter(|c| c.enabled) {
        Some(c) => format!("jar:{}", c.key()),
        None => String::new(),
    }
}

/// 把所有 jar 的未落盘变更写下去（运行收尾时调）。
pub fn flush_all() {
    let jars: Vec<Arc<CookieJar>> = registry().values().cloned().collect();
    for j in jars {
        j.flush();
    }
}

/// 丢弃进程内缓存的 jar（测试用；下次取用会重新从磁盘加载）。
pub fn reset_registry() {
    registry().clear();
}

// ── 请求侧 ──────────────────────────────────────────

fn has_cookie_header(req: &HttpRequest) -> bool {
    req.headers.iter().any(|h| h.key.trim().eq_ignore_ascii_case("cookie"))
}

/// 给请求补上 jar 里匹配的 `Cookie` 头，返回是否补了。
///
/// **用户手写的 `Cookie` 头优先**：显式写了就照它发，jar 不插手（同 Postman；
/// 也是"用户显式意图 > 自动行为"的一般原则）。reqwest 侧同样只在无该头时才补，
/// 故这里补过之后不会被 provider 再叠一次。
pub fn attach(req: &mut HttpRequest, cfg: &ClientConfig) -> bool {
    if has_cookie_header(req) {
        return false;
    }
    let Some(jar) = jar_for(cfg) else { return false };
    let Some(v) = jar.header_for(&req.url) else { return false };
    req.headers.push(KvPair::new("Cookie", v));
    true
}

/// Digest 重发前刷新自动带上的 `Cookie` 头——首发的 401 响应常常同时下发新会话。
/// 只在头是我们自己加的时候才动它（`attached`），用户手写的照旧不碰。
pub fn refresh(req: &mut HttpRequest, cfg: &ClientConfig, attached: bool) {
    if !attached {
        return;
    }
    req.headers.retain(|h| !h.key.trim().eq_ignore_ascii_case("cookie"));
    attach(req, cfg);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用配置：开关 + jar 路径
    fn cfg_with(enabled: bool, jar: Option<String>) -> ClientConfig {
        ClientConfig {
            cookies: Some(CookieConfig { enabled, jar_path: jar }),
            ..Default::default()
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("apicase-cookie-test-{name}"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn jar_with(url: &str, set_cookie: &[&str], path: Option<&Path>) -> Arc<CookieJar> {
        let jar = Arc::new(CookieJar::load(path.map(Path::to_path_buf)));
        let u = Url::parse(url).expect("URL");
        let raw: Vec<RawCookie<'static>> =
            set_cookie.iter().filter_map(|s| RawCookie::parse(s.to_string()).ok()).collect();
        jar.write_store().store_response_cookies(raw.into_iter(), &u);
        jar.mark_dirty();
        jar
    }

    /// 存进去的 cookie 要能按域/路径取回来，且**不发给别的域**
    #[test]
    fn stores_and_matches_by_domain() {
        let jar = jar_with("https://api.example.com/v1/login", &["sid=abc; Path=/"], None);
        assert_eq!(jar.header_for("https://api.example.com/v1/me").as_deref(), Some("sid=abc"));
        assert_eq!(jar.header_for("https://other.com/v1/me"), None, "别的域不该拿到");
    }

    /// 多个 Set-Cookie 一并收下，回带时拼成一个头
    #[test]
    fn multiple_cookies_join_into_one_header() {
        let jar = jar_with("http://localhost/api", &["a=1", "b=2"], None);
        let h = jar.header_for("http://localhost/api").expect("应有 cookie");
        assert!(h.contains("a=1") && h.contains("b=2") && h.contains("; "), "实际 {h}");
    }

    /// 落盘 → 重新加载，会话仍在（会话 cookie 也要留住）
    #[test]
    fn persists_across_reload() {
        let path = tmp_path("persist.json");
        let jar = jar_with("http://localhost/api", &["sid=xyz"], Some(&path));
        jar.flush();
        assert!(path.exists(), "应已落盘");

        let again = CookieJar::load(Some(path.clone()));
        assert_eq!(again.header_for("http://localhost/api").as_deref(), Some("sid=xyz"));
        let _ = std::fs::remove_file(&path);
    }

    /// 坏掉的 jar 文件不该让请求发不出去，最坏只是重新登录一次
    #[test]
    fn broken_file_falls_back_to_empty() {
        let path = tmp_path("broken.json");
        std::fs::write(&path, b"{ not json at all").expect("写文件");
        let jar = CookieJar::load(Some(path.clone()));
        assert!(jar.list().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// 列表 / 删一条 / 按域清 / 全清
    #[test]
    fn manage_list_remove_clear() {
        let jar = jar_with("https://a.test/x", &["k1=v1; Path=/", "k2=v2; Path=/"], None);
        let u = Url::parse("https://b.test/y").expect("URL");
        let raw: Vec<RawCookie<'static>> = vec![RawCookie::parse("k3=v3".to_string()).expect("解析")];
        jar.write_store().store_response_cookies(raw.into_iter(), &u);

        let items = jar.list();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].domain, "a.test", "按域排序");
        assert!(items[0].expires_ms.is_none(), "无 Expires 即会话 cookie");

        let it = items[0].clone();
        assert!(jar.remove(&it.domain, &it.path, &it.name));
        assert!(!jar.remove(&it.domain, &it.path, &it.name), "删过了就不该再有");
        assert_eq!(jar.list().len(), 2);

        assert_eq!(jar.clear(Some("a.test")), 1);
        assert_eq!(jar.list().len(), 1, "只该清掉 a.test 那条");
        assert_eq!(jar.clear(None), 1);
        assert!(jar.list().is_empty());
    }

    fn input(domain: &str, name: &str, value: &str) -> CookieInput {
        CookieInput {
            domain: domain.into(),
            path: String::new(),
            name: name.into(),
            value: value.into(),
            secure: false,
            http_only: false,
            expires_ms: None,
        }
    }

    /// 手工加一条：能存下、能按域取回、路径缺省补 `/`
    #[test]
    fn put_adds_a_cookie() {
        let jar = Arc::new(CookieJar::load(None));
        jar.put(None, &input("api.test", "sid", "abc")).expect("应能保存");

        let items = jar.list();
        assert_eq!(items.len(), 1);
        assert_eq!((items[0].name.as_str(), items[0].value.as_str()), ("sid", "abc"));
        assert_eq!(items[0].path, "/", "路径缺省补 /");
        assert!(items[0].host_only, "没写前导点就是 host-only");
        assert_eq!(jar.header_for("http://api.test/x").as_deref(), Some("sid=abc"));
    }

    /// 带前导点 = 子域一并生效，且这一点在列表里要看得出来（否则编辑一次就丢了）
    #[test]
    fn put_with_leading_dot_covers_subdomains() {
        let jar = Arc::new(CookieJar::load(None));
        jar.put(None, &input(".test.com", "t", "1")).expect("应能保存");

        assert!(!jar.list()[0].host_only, "带 Domain 属性不是 host-only");
        assert_eq!(jar.header_for("http://a.test.com/").as_deref(), Some("t=1"), "子域也该带上");
        assert_eq!(jar.header_for("http://test.com/").as_deref(), Some("t=1"));
    }

    /// 改主键（域 / 路径 / 名）要把旧的那条删掉，不能留孤儿
    #[test]
    fn put_replaces_the_previous_key() {
        let jar = Arc::new(CookieJar::load(None));
        jar.put(None, &input("api.test", "sid", "old")).expect("应能保存");
        let prev = CookieKey { domain: "api.test".into(), path: "/".into(), name: "sid".into() };

        // 只改值：仍是一条
        jar.put(Some(&prev), &input("api.test", "sid", "new")).expect("应能保存");
        assert_eq!(jar.list().len(), 1);
        assert_eq!(jar.list()[0].value, "new");

        // 改名：旧的必须消失
        jar.put(Some(&prev), &input("api.test", "token", "new")).expect("应能保存");
        let items = jar.list();
        assert_eq!(items.len(), 1, "改名后不该留下两条：{items:?}");
        assert_eq!(items[0].name, "token");
    }

    /// 过期时间写进去要读得回来（`Expires=` 走 HTTP 日期，星期几算错就会被解析器丢掉）
    #[test]
    fn put_keeps_the_expiry() {
        let jar = Arc::new(CookieJar::load(None));
        // 取整秒的将来时刻：`Expires=` 只到秒，写死一个日期又会随着时间推移变成"过去"
        let at = (crate::util::now_ms() / 1000 + 86_400) * 1000;
        jar.put(None, &CookieInput { expires_ms: Some(at), ..input("api.test", "k", "v") })
            .expect("应能保存");
        assert_eq!(jar.list()[0].expires_ms, Some(at));
    }

    /// 填错时给的是能直接照做的提示，而不是解析器的英文原文
    #[test]
    fn put_rejects_bad_input() {
        let jar = Arc::new(CookieJar::load(None));
        let err = |i: CookieInput| jar.put(None, &i).expect_err("应报错");

        assert!(err(input("api.test", "  ", "v")).contains("名不能为空"));
        assert!(err(input("  ", "k", "v")).contains("域不能为空"));
        assert!(err(CookieInput { path: "x".into(), ..input("api.test", "k", "v") }).contains("/ 开头"));
        // 过期时间在过去：cookie 存不下，要说清楚而不是让用户对着"保存了却没有"发懵
        let past = err(CookieInput { expires_ms: Some(1_000), ..input("api.test", "k", "v") });
        assert!(past.contains("过期时间"), "实际 {past}");
        assert!(jar.list().is_empty(), "报错的这几条都不该留下痕迹");
    }

    /// 用户手写的 Cookie 头优先，jar 不插手
    #[test]
    fn manual_cookie_header_wins() {
        let path = tmp_path("attach.json");
        let jar = jar_with("http://localhost/api", &["sid=fromjar"], Some(&path));
        jar.flush();
        reset_registry();

        let cfg = cfg_with(true, Some(path.to_string_lossy().into_owned()));
        let mut req = HttpRequest {
            method: "GET".into(),
            url: "http://localhost/api".into(),
            headers: vec![KvPair::new("Cookie", "sid=manual")],
            body: None,
        };
        assert!(!attach(&mut req, &cfg), "已有 Cookie 头就不该再补");
        assert_eq!(req.headers.len(), 1);
        assert_eq!(req.headers[0].value, "sid=manual");

        let mut bare = HttpRequest {
            method: "GET".into(),
            url: "http://localhost/api".into(),
            headers: vec![],
            body: None,
        };
        assert!(attach(&mut bare, &cfg), "没有就该补上");
        assert_eq!(bare.headers[0].value, "sid=fromjar");

        reset_registry();
        let _ = std::fs::remove_file(&path);
    }

    /// 开关关闭：不取 jar，也就不会带 cookie
    #[test]
    fn disabled_switch_attaches_nothing() {
        let cfg = cfg_with(false, None);
        assert!(jar_for(&cfg).is_none());
        assert!(client_key(&cfg).is_empty());

        let mut req = HttpRequest {
            method: "GET".into(),
            url: "http://localhost/api".into(),
            headers: vec![],
            body: None,
        };
        assert!(!attach(&mut req, &cfg));
        assert!(req.headers.is_empty());
    }

    // ── 端到端：真的把请求发出去 ────────────────────
    //
    // cookie 的价值全在"服务端下发了、下一个请求带上了"这条链路上，
    // 只测 store 的读写等于把被测对象换掉了。

    /// 开发机上常设 HTTPS_PROXY，不显式直连就根本到不了本地 mock
    fn e2e_cfg(jar: &Path, enabled: bool) -> ClientConfig {
        ClientConfig {
            proxy: Some(crate::http::ProxyConfig { mode: "none".into(), url: None }),
            options: None,
            cookies: Some(CookieConfig {
                enabled,
                jar_path: Some(jar.to_string_lossy().into_owned()),
            }),
        }
    }

    async fn login_then_me(jar: &Path, enabled: bool) -> (crate::testutil::MockServer, Vec<KvPair>) {
        let srv = crate::testutil::MockServer::start(|req| match req.path.as_str() {
            "/login" => crate::testutil::Reply::json("{}")
                .with_header("Set-Cookie", "sid=s3cret; Path=/")
                .with_header("Set-Cookie", "theme=dark; Path=/"),
            _ => crate::testutil::Reply::json("{}"),
        })
        .await;
        let cfg = e2e_cfg(jar, enabled);
        let get = |url: String| crate::model::HttpSpec { method: "GET".into(), url, ..Default::default() };

        crate::auth::send_with_auth(&get(format!("{}/login", srv.base)), &cfg)
            .await
            .expect("登录请求应成功");
        let (sent, _) = crate::auth::send_with_auth(&get(format!("{}/me", srv.base)), &cfg)
            .await
            .expect("第二个请求应成功");
        (srv, sent.headers)
    }

    /// 服务端下发的 Set-Cookie 要在下一个请求上原样带回去——本需求的主线
    #[tokio::test]
    async fn set_cookie_comes_back_on_the_next_request() {
        let path = tmp_path("e2e-on.json");
        reset_registry();
        crate::http::clear_client_cache();

        let (srv, sent) = login_then_me(&path, true).await;

        let got = srv.requests();
        let cookie = got[1].header("cookie").expect("第二个请求应带上 Cookie 头");
        assert!(cookie.contains("sid=s3cret"), "实际 {cookie}");
        assert!(cookie.contains("theme=dark"), "两个 Set-Cookie 都该带回去：{cookie}");
        assert!(got[0].header("cookie").is_none(), "首个请求那时 jar 还是空的");

        // 报告如实：请求记录里看得见本次带了什么会话
        let recorded = sent.iter().find(|h| h.key.eq_ignore_ascii_case("cookie")).expect("请求记录里应有 Cookie 头");
        assert!(recorded.value.contains("sid=s3cret"), "实际 {}", recorded.value);

        reset_registry();
        crate::http::clear_client_cache();
        let _ = std::fs::remove_file(&path);
    }

    /// 开关关掉：既不带也不收，jar 文件也不该被创建
    #[tokio::test]
    async fn switch_off_sends_nothing() {
        let path = tmp_path("e2e-off.json");
        reset_registry();
        crate::http::clear_client_cache();

        let (srv, sent) = login_then_me(&path, false).await;

        assert!(srv.requests()[1].header("cookie").is_none(), "关掉开关就不该带 cookie");
        assert!(!sent.iter().any(|h| h.key.eq_ignore_ascii_case("cookie")));
        assert!(!path.exists(), "关闭时不该写出 jar 文件");

        reset_registry();
        crate::http::clear_client_cache();
    }

    /// 同一路径全进程共用一份 store——两份各写各的，谁后落盘谁把对方抹掉
    #[test]
    fn same_path_shares_one_jar() {
        let path = tmp_path("shared.json");
        let p = path.to_string_lossy().into_owned();
        reset_registry();
        let a = jar_at(Some(&p));
        let b = jar_at(Some(&p));
        assert!(Arc::ptr_eq(&a, &b));
        reset_registry();
        let _ = std::fs::remove_file(&path);
    }
}
