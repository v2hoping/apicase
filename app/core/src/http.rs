//! 发送层：reqwest 客户端的构建 / 缓存与一次真实的 HTTP 往返。
//!
//! # 客户端缓存是本模块存在的主要理由
//!
//! 改造前每发一个请求都 `Client::builder().build()` 一次。这件事有两笔开销：
//!
//! 1. **TLS 根证书库**每次重建（解析上百张 webpki 根证书）；
//! 2. **连接池**随客户端一起丢弃——批量跑 100 个打同一个服务的用例，
//!    就是 100 次 TCP 握手 + 100 次 TLS 握手，而它们本可以复用一条连接。
//!
//! 第 2 条在回归运行里是数量级的差别，也是"把执行下沉到 Rust"最直接的性能收益。
//! 客户端按「代理 + 证书校验 + CA + 超时」这组配置缓存；配置不变就一直复用同一个，
//! keep-alive 连接因此能真正活下来。
//!
//! CA 文件的 **mtime 与长度**参与缓存键：用户换了证书文件、客户端会自动重建，
//! 不必重启应用——只用路径做键的话，改完证书还得重启才生效，很难想到原因。

use crate::report::KvPair;
use crate::request::{HttpRequest, RequestBody};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 响应体读取上限。API 调试的响应体极少超过几 MB；真撞上这条线（比如误把下载接口
/// 当成 API 来跑）应当**明确报错而不是静默截断**——静默截断会让人对着一份缺尾巴的
/// JSON 排查半天。
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// 代理设置（前端「设置 → 代理」）。
/// `system`（或缺省）= 跟随系统（reqwest 读 `HTTP(S)_PROXY` 环境变量）；
/// `none` = 直连；`custom` = 指定地址。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    pub mode: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// 工作空间级请求设置（前端「设置 → 通用」，存于 `application.yml` 的 `settings:`）。
/// 三项均可选：缺省即「校验开启 / 无自定义 CA / 不限超时」这套安全侧默认。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOptions {
    /// None 或 true = 校验；false = 接受任何证书（降安全）
    #[serde(default)]
    pub verify_ssl: Option<bool>,
    /// 自定义 CA 证书的**绝对路径**（相对路径由调用方 join 工作空间根后传入）
    #[serde(default)]
    pub ca_cert_path: Option<String>,
    /// 整个请求的超时上限（毫秒）；None 或 0 = 不限制
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// 一次执行用到的全部客户端级配置。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientConfig {
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
    #[serde(default)]
    pub options: Option<RequestOptions>,
}

/// 一次 HTTP 往返的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<KvPair>,
    pub body: String,
    /// 请求发出到响应体读完的耗时
    pub elapsed_ms: u64,
}

// ── 客户端缓存 ──────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClientKey {
    proxy_mode: String,
    proxy_url: String,
    verify_ssl: bool,
    ca_path: String,
    /// CA 文件的 (mtime 毫秒, 字节数)——换了证书文件即自动重建客户端
    ca_stamp: (u64, u64),
    timeout_ms: u64,
}

/// 缓存条目上限。配置组合来自"代理 × 证书 × 超时"，正常用法下只有个位数；
/// 定个上限纯粹是防御——真涨到这个数说明有人在循环里改配置，清空重来即可。
const MAX_CACHED_CLIENTS: usize = 16;

/// 按配置复用 reqwest 客户端的池子。
///
/// 做成结构体而非一组自由函数，是为了让测试能拿独立实例——否则测试之间会
/// 通过全局单例互相看见对方的条目，并行跑就开始随机失败。
#[derive(Default)]
pub struct ClientPool {
    map: Mutex<HashMap<ClientKey, reqwest::Client>>,
}

impl ClientPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// **锁中毒不能让整个池子报废**。持锁期间别处 panic 会毒化这把锁，
    /// 若照直 `unwrap`，之后每一个请求都会以"缓存已损坏"告终——一次无关的
    /// panic 就把发请求这个核心功能永久打死了。池里存的只是可重建的客户端，
    /// 没有需要保护的不变量，直接取回内部数据继续用即可。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ClientKey, reqwest::Client>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 取（或构建）与该配置匹配的客户端。`reqwest::Client` 内部是 `Arc`，clone 极廉价。
    pub fn get(&self, cfg: &ClientConfig) -> Result<reqwest::Client, String> {
        let key = key_of(cfg);
        if let Some(c) = self.lock().get(&key) {
            return Ok(c.clone());
        }
        // 构建放在锁外：解析 CA、装配 TLS 都要花时间，不该把别的请求堵在门口
        let client = build_client(cfg)?;
        let mut map = self.lock();
        if map.len() >= MAX_CACHED_CLIENTS {
            map.clear();
        }
        // 可能有并发者先插入了同一个键——用它的，保证同配置全局只有一个连接池
        Ok(map.entry(key).or_insert(client).clone())
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn pool() -> &'static ClientPool {
    static POOL: OnceLock<ClientPool> = OnceLock::new();
    POOL.get_or_init(ClientPool::new)
}

fn key_of(cfg: &ClientConfig) -> ClientKey {
    let (proxy_mode, proxy_url) = match cfg.proxy.as_ref() {
        Some(p) => (p.mode.clone(), p.url.clone().unwrap_or_default()),
        None => (String::new(), String::new()),
    };
    let opts = cfg.options.clone().unwrap_or_default();
    let ca_path = opts.ca_cert_path.unwrap_or_default().trim().to_string();
    let ca_stamp = if ca_path.is_empty() { (0, 0) } else { file_stamp(&ca_path) };
    ClientKey {
        proxy_mode,
        proxy_url,
        verify_ssl: opts.verify_ssl != Some(false),
        ca_path,
        ca_stamp,
        timeout_ms: opts.timeout_ms.unwrap_or(0),
    }
}

/// 文件的 (mtime 毫秒, 长度)。读不到就返回 (0,0)——此时缓存键退化为仅按路径，
/// 后续 `build_client` 读盘时会给出真正的错误信息。
fn file_stamp(path: &str) -> (u64, u64) {
    let Ok(md) = std::fs::metadata(path) else { return (0, 0) };
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    (mtime, md.len())
}

/// 取（或构建）进程级共享池里与该配置匹配的客户端。
pub fn client_for(cfg: &ClientConfig) -> Result<reqwest::Client, String> {
    pool().get(cfg)
}

/// 清空客户端缓存（改证书 / 改代理后想立刻生效时用）。
pub fn clear_client_cache() {
    pool().clear();
}

fn build_client(cfg: &ClientConfig) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder()
        // 服务端日志里能一眼看出流量来源；请求级的同名头仍可覆盖它
        .user_agent(concat!("apicase/", env!("CARGO_PKG_VERSION")))
        // 空闲连接留久一点：批量运行里同一个服务会被连续打很多次
        .pool_idle_timeout(Duration::from_secs(90));

    match cfg.proxy.as_ref().map(|p| p.mode.as_str()) {
        Some("none") => b = b.no_proxy(),
        Some("custom") => {
            let url = cfg
                .proxy
                .as_ref()
                .and_then(|p| p.url.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match url {
                Some(u) => {
                    let px = reqwest::Proxy::all(u).map_err(|e| format!("代理地址非法: {e}"))?;
                    b = b.proxy(px);
                }
                // custom 但没填地址 → 视为直连，而不是悄悄回落到系统代理
                None => b = b.no_proxy(),
            }
        }
        // system / 缺省：交给 reqwest 默认（读 HTTP(S)_PROXY 环境变量）
        _ => {}
    }

    if let Some(opts) = cfg.options.as_ref() {
        // 仅显式 false 才关闭；此时跳过信任链、有效期与域名匹配（流量仍加密，但不再验证对端身份）
        if opts.verify_ssl == Some(false) {
            b = b.danger_accept_invalid_certs(true);
        }
        if let Some(ca) = opts.ca_cert_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            // add_root_certificate 是**追加**到默认信任库，公网 HTTPS 的校验不受影响
            for cert in load_ca_certificates(ca)? {
                b = b.add_root_certificate(cert);
            }
        }
        // 覆盖「连接 + 发送 + 读完响应体」全过程，不是单纯的连接超时
        if let Some(ms) = opts.timeout_ms.filter(|ms| *ms > 0) {
            b = b.timeout(Duration::from_millis(ms));
        }
    }

    b.build().map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

const PEM_MARKER: &[u8] = b"-----BEGIN CERTIFICATE-----";

/// 读取 CA 证书文件并解析（PEM 与 DER 两族都支持）。
/// 任何一步失败都**明确报错**——静默忽略会让用户以为配好了却仍连不上，最难排查。
pub fn load_ca_certificates(path: &str) -> Result<Vec<reqwest::Certificate>, String> {
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

// ── 发送 ────────────────────────────────────────────

/// 发出一次请求并读完响应。由 Rust 直接发，天然绕过浏览器 CORS。
pub async fn send(req: &HttpRequest, cfg: &ClientConfig) -> Result<HttpResponse, String> {
    let url = req.url.trim();
    if url.is_empty() {
        return Err("URL 不能为空".to_string());
    }
    let method = reqwest::Method::from_bytes(req.method.trim().to_uppercase().as_bytes())
        .map_err(|_| format!("非法的 HTTP 方法: {}", req.method))?;

    let client = client_for(cfg)?;
    let mut b = client.request(method, url);
    for h in &req.headers {
        if h.key.trim().is_empty() {
            continue;
        }
        b = b.header(h.key.trim(), h.value.as_str());
    }

    match &req.body {
        Some(RequestBody::Form(parts)) => {
            // multipart 自带 Content-Type（含 boundary），故放在 header 之后设置，覆盖手填的同名头
            let mut form = reqwest::multipart::Form::new();
            for p in parts {
                match p.file_path.as_deref() {
                    Some(path) => {
                        let bytes = std::fs::read(path)
                            // 多文件表单里只报路径不好定位是哪一行，带上字段名
                            .map_err(|e| format!("读取表单文件失败（{} → {path}）: {e}", p.name))?;
                        let name = p.file_name.clone().unwrap_or_else(|| crate::request::base_name(path));
                        let mut part = reqwest::multipart::Part::bytes(bytes).file_name(name);
                        if let Some(ct) = p.content_type.as_deref().filter(|s| !s.is_empty()) {
                            part = part
                                .mime_str(ct)
                                .map_err(|e| format!("表单文件 Content-Type 非法（{} → {ct}）: {e}", p.name))?;
                        }
                        form = form.part(p.name.clone(), part);
                    }
                    None => form = form.text(p.name.clone(), p.value.clone()),
                }
            }
            b = b.multipart(form);
        }
        Some(RequestBody::File(path)) => {
            let bytes = std::fs::read(path).map_err(|e| format!("读取请求体文件失败（{path}）: {e}"))?;
            b = b.body(bytes);
        }
        Some(RequestBody::Text(t)) if !t.is_empty() => b = b.body(t.clone()),
        _ => {}
    }

    let start = Instant::now();
    let resp = b.send().await.map_err(|e| format!("请求失败: {e}"))?;

    let status = resp.status();
    let headers: Vec<KvPair> = resp
        .headers()
        .iter()
        .map(|(k, v)| KvPair::new(k.as_str(), v.to_str().unwrap_or("")))
        .collect();
    let body = read_body(resp).await?;
    Ok(HttpResponse {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        body,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

/// 流式读响应体并守住上限。
///
/// 不用 `resp.text()`：那是一次性全读，遇到超大响应会先把内存吃光才轮到我们判断。
/// 分块累加能在**跨过上限的那一刻**就停手，连接随即丢弃。
async fn read_body(mut resp: reqwest::Response) -> Result<String, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(resp.content_length().unwrap_or(0).min(1 << 20) as usize);
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("读取响应体失败: {e}"))? {
        if buf.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(format!(
                "响应体超过 {} MB 上限，已中止读取（apicase 用于接口调试，不适合拉取大文件）",
                MAX_RESPONSE_BYTES / 1024 / 1024
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    // 非 UTF-8 字节用替换字符兜住，不因为一个坏字节丢掉整个响应
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::HttpRequest;
    use std::sync::Arc;

    fn req(url: &str) -> HttpRequest {
        HttpRequest { method: "GET".into(), url: url.into(), headers: vec![], body: None }
    }

    #[tokio::test]
    async fn rejects_empty_url_and_bad_method() {
        assert!(send(&req("   "), &ClientConfig::default()).await.is_err());
        let mut r = req("https://example.com");
        r.method = "BAD METHOD".into();
        assert!(send(&r, &ClientConfig::default()).await.is_err());
    }

    #[tokio::test]
    async fn missing_body_file_is_reported_clearly() {
        let mut r = req("https://example.com");
        r.method = "POST".into();
        r.body = Some(RequestBody::File(
            std::env::temp_dir().join("apicase-not-exist-body.bin").to_string_lossy().into_owned(),
        ));
        let err = send(&r, &ClientConfig::default()).await.expect_err("应报错");
        assert!(err.contains("读取请求体文件失败"), "错误应指明是请求体文件读取失败: {err}");
    }

    /// 多文件表单里只报路径不好定位是哪一行，错误信息要带字段名
    #[tokio::test]
    async fn missing_form_file_error_names_the_field() {
        let mut r = req("https://example.com");
        r.method = "POST".into();
        r.body = Some(RequestBody::Form(vec![crate::request::FormPart {
            name: "avatar".into(),
            value: String::new(),
            file_path: Some(std::env::temp_dir().join("apicase-not-exist-form.png").to_string_lossy().into_owned()),
            file_name: Some("a.png".into()),
            content_type: Some("image/png".into()),
        }]));
        let err = send(&r, &ClientConfig::default()).await.expect_err("应报错");
        assert!(err.contains("读取表单文件失败"), "{err}");
        assert!(err.contains("avatar"), "错误信息应带上字段名: {err}");
    }

    /// 同一份配置必须拿到同一个客户端——否则连接池形同虚设，
    /// 批量跑 100 个打同一服务的用例就是 100 次 TCP + TLS 握手。
    #[test]
    fn same_config_reuses_one_client() {
        let p = ClientPool::new();
        let cfg = ClientConfig {
            proxy: Some(ProxyConfig { mode: "none".into(), url: None }),
            options: Some(RequestOptions { timeout_ms: Some(1000), ..Default::default() }),
        };
        for _ in 0..3 {
            p.get(&cfg).expect("应能构建");
        }
        assert_eq!(p.len(), 1, "三次取用只该落一个条目");
    }

    /// 配置不同必须是不同的客户端（否则关了证书校验的设置会污染别的请求）
    #[test]
    fn different_configs_get_different_clients() {
        let p = ClientPool::new();
        let configs = [
            ClientConfig::default(),
            ClientConfig { proxy: Some(ProxyConfig { mode: "none".into(), url: None }), options: None },
            ClientConfig {
                options: Some(RequestOptions { verify_ssl: Some(false), ..Default::default() }),
                ..Default::default()
            },
            ClientConfig {
                options: Some(RequestOptions { timeout_ms: Some(500), ..Default::default() }),
                ..Default::default()
            },
        ];
        for c in &configs {
            p.get(c).expect("应能构建");
        }
        assert_eq!(p.len(), configs.len());
    }

    /// 缓存不该无限增长
    #[test]
    fn cache_is_bounded() {
        let p = ClientPool::new();
        for i in 0..MAX_CACHED_CLIENTS + 3 {
            let cfg = ClientConfig {
                options: Some(RequestOptions { timeout_ms: Some(i as u64 + 1), ..Default::default() }),
                ..Default::default()
            };
            p.get(&cfg).expect("应能构建");
        }
        assert!(p.len() <= MAX_CACHED_CLIENTS, "实际 {}", p.len());
    }

    /// 别处 panic 毒化了这把锁之后，池子必须还能继续工作——
    /// 否则一次无关的 panic 就把「发请求」这个核心功能永久打死了。
    #[test]
    fn poisoned_lock_does_not_break_the_pool() {
        let p = Arc::new(ClientPool::new());
        let cfg = ClientConfig::default();
        p.get(&cfg).expect("应能构建");

        let p2 = p.clone();
        let _ = std::thread::spawn(move || {
            let _g = p2.map.lock().unwrap();
            panic!("故意在持锁时崩掉");
        })
        .join();

        assert!(p.map.is_poisoned(), "锁应已中毒");
        assert_eq!(p.len(), 1, "中毒后仍读得到");
        p.get(&cfg).expect("中毒后仍能取用客户端");
        p.clear();
    }

    /// 换了 CA 证书文件内容，客户端要自动重建——否则改完证书还得重启，没人想得到
    #[test]
    fn ca_file_change_invalidates_the_key() {
        let path = std::env::temp_dir().join("apicase-key-stamp.pem");
        std::fs::write(&path, b"a").expect("写文件");
        let cfg = ClientConfig {
            options: Some(RequestOptions {
                ca_cert_path: Some(path.to_string_lossy().into_owned()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let k1 = key_of(&cfg);
        std::fs::write(&path, b"changed content").expect("写文件");
        let k2 = key_of(&cfg);
        assert_ne!(k1, k2, "文件内容变了，缓存键必须跟着变");
        let _ = std::fs::remove_file(&path);
    }

    /// 测试用自签 CA（`openssl req -x509 -newkey rsa:2048 -days 36500 -subj "/CN=apicase-test-ca"`）。
    /// 只用于验证「能被解析并装进 ClientBuilder」，不参与任何真实握手。
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

    #[test]
    fn ca_certificate_loading() {
        let base = std::env::temp_dir().join("apicase-ca-test-core");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("建测试目录");
        let p = |n: &str| base.join(n).to_string_lossy().into_owned();

        std::fs::write(base.join("ca.crt"), TEST_CA_PEM).expect("写证书");
        assert_eq!(load_ca_certificates(&p("ca.crt")).expect("单张 PEM 应解析成功").len(), 1);

        // bundle：一个文件里串多张（CA 链的常见形态）
        std::fs::write(base.join("chain.pem"), format!("{TEST_CA_PEM}{TEST_CA_PEM}")).expect("写证书");
        assert_eq!(load_ca_certificates(&p("chain.pem")).expect("bundle 应解析成功").len(), 2);

        let err = load_ca_certificates(&p("nope.crt")).expect_err("应报错");
        assert!(err.contains("读取 CA 证书失败"), "错误应指明是读取阶段失败: {err}");

        // 内容不是证书 → 必须明确报错，不能静默忽略
        std::fs::write(base.join("junk.crt"), b"not a certificate at all").expect("写文件");
        let err = load_ca_certificates(&p("junk.crt")).expect_err("应报错");
        assert!(err.contains("解析 CA 证书失败"), "错误应指明是解析阶段失败: {err}");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 坏 CA 路径必须让请求**提前失败**，而不是丢掉设置照常发出去
    #[tokio::test]
    async fn bad_ca_path_fails_the_request() {
        clear_client_cache();
        let cfg = ClientConfig {
            options: Some(RequestOptions {
                ca_cert_path: Some(
                    std::env::temp_dir().join("apicase-absent-ca.crt").to_string_lossy().into_owned(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = send(&req("https://example.com"), &cfg).await.expect_err("应报错");
        assert!(err.contains("CA 证书"), "错误应指明与 CA 证书有关: {err}");
        clear_client_cache();
    }

    /// 真实 GET（需联网）
    #[tokio::test]
    #[ignore]
    async fn real_get_request_succeeds() {
        let resp = send(&req("https://example.com"), &ClientConfig::default()).await.expect("请求应成功");
        assert_eq!(resp.status, 200);
        assert!(!resp.body.is_empty());
    }
}
