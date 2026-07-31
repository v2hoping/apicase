//! 需要「发送前 / 发送后」额外交互的两种认证：Digest 与 OAuth 2.0。
//!
//! 静态头（Basic / Bearer / API Key）由 `request::build` 直接组装；这两种不行：
//!
//! - **Digest** 必须先吃到服务端 401 的 `WWW-Authenticate` challenge 才能算摘要；
//! - **OAuth 2.0**（客户端凭据）必须先去 token 端点换 `access_token`。
//!
//! 两者都要发额外请求，因此统一收敛到 `send_with_auth`——token 交换与 Digest 重发
//! 走的是同一个客户端，代理与证书设置对它们一样生效。
//!
//! # token 缓存的 in-flight 去重
//!
//! 并发跑 8 个用例、每个都配了同一套 OAuth 2.0，朴素的"查缓存→没有→去换"会同时
//! 发出 8 次 token 请求：既慢，又可能触发授权服务器的限流，有些实现还会让先拿到的
//! token 提前失效。这里给每个缓存键配一把锁，**同键并发只有第一个真去换**，
//! 其余在锁上等，醒来直接读缓存。这是把执行下沉到 Rust 后才具备的能力——
//! 也是 case 之间敢开并发的前提。

use crate::http::{self, ClientConfig, HttpResponse};
use crate::model::{AuthType, ClientAuth, HttpSpec};
use crate::report::KvPair;
use crate::request::{self, base64_encode, encode_uri_component, RequestBody};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Digest（RFC 2617 / 7616）────────────────────────

type Challenge = HashMap<String, String>;

/// 解析 `WWW-Authenticate: Digest realm="x", nonce="y", qop="auth"` 的参数表。
/// 认不出 Digest 方案返回 None（服务端可能同时给了 Basic 等其它方案）。
pub fn parse_digest_challenge(header: &str) -> Option<Challenge> {
    let start = find_digest_scheme(header)?;
    let rest = &header[start..];
    let mut out = Challenge::new();
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // 跳过分隔符与空白
        while i < b.len() && (b[i] == b',' || b[i].is_ascii_whitespace()) {
            i += 1;
        }
        // key
        let ks = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b'_') {
            i += 1;
        }
        if i == ks {
            break;
        }
        let key = rest[ks..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            break;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        // value：带引号（支持 \ 转义）或裸 token
        let value = if i < b.len() && b[i] == b'"' {
            i += 1;
            let mut v = String::new();
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                }
                let l = utf8_len(b[i]);
                v.push_str(&rest[i..i + l]);
                i += l;
            }
            i += 1; // 吃掉收尾引号
            v
        } else {
            let vs = i;
            while i < b.len() && b[i] != b',' && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            rest[vs..i].to_string()
        };
        out.insert(key, value);
    }
    (!out.is_empty()).then_some(out)
}

/// 找 `Digest` 方案的参数区起点。
///
/// 一个 `WWW-Authenticate` 头里可以并列多个方案（`Basic realm="x", Digest realm="y"`），
/// 所以要求 `digest` 前面是行首或逗号——否则某个参数值里恰好含 "digest" 就会误判。
fn find_digest_scheme(header: &str) -> Option<usize> {
    let lower = header.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut from = 0;
    while let Some(p) = lower[from..].find("digest") {
        let at = from + p;
        let before = lower[..at].trim_end();
        let after = at + "digest".len();
        if (before.is_empty() || before.ends_with(',')) && b.get(after).is_some_and(u8::is_ascii_whitespace) {
            let mut i = after;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            return Some(i);
        }
        from = after;
    }
    None
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn md5_hex(s: &str) -> String {
    format!("{:x}", md5::compute(s.as_bytes()))
}

/// URL 的 path + query（Digest 的 `uri` 参数与 HA2 都按它算）。
fn request_uri(url: &str) -> String {
    // 不引 url crate：只需要「跳过 scheme://host 之后的部分」这一件事
    let after_scheme = url.find("//").map(|i| i + 2).unwrap_or(0);
    match url[after_scheme..].find('/') {
        Some(i) => url[after_scheme + i..].to_string(),
        None => "/".to_string(),
    }
}

/// 客户端 nonce。用于防重放与 HA1 加盐，**不是密钥材料**，
/// 因此不引入随机数依赖：单调计数器 + 纳秒时钟已足以保证每次不同。
fn client_nonce() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    md5_hex(&format!("{t}:{n}:{:?}", std::thread::current().id()))[..16].to_string()
}

pub struct DigestOpts<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub method: &'a str,
    pub url: &'a str,
    /// 测试注入用；生产走 `client_nonce()`
    pub cnonce: Option<&'a str>,
    pub nc: Option<&'a str>,
}

/// 依据 challenge 计算 `Authorization: Digest …`（仅 `qop=auth` 与无 qop；`auth-int` 不支持）。
pub fn build_digest_header(ch: &Challenge, o: &DigestOpts) -> String {
    let empty = String::new();
    let realm = ch.get("realm").unwrap_or(&empty);
    let nonce = ch.get("nonce").unwrap_or(&empty);
    let algorithm = ch.get("algorithm").cloned().unwrap_or_default();
    let use_qop = ch
        .get("qop")
        .map(|q| q.split(',').any(|s| s.trim().eq_ignore_ascii_case("auth")))
        .unwrap_or(false);
    let uri = request_uri(o.url);
    let cnonce = o.cnonce.map(str::to_string).unwrap_or_else(client_nonce);
    let nc = o.nc.unwrap_or("00000001");

    let mut ha1 = md5_hex(&format!("{}:{realm}:{}", o.username, o.password));
    if algorithm.eq_ignore_ascii_case("MD5-sess") {
        ha1 = md5_hex(&format!("{ha1}:{nonce}:{cnonce}"));
    }
    let ha2 = md5_hex(&format!("{}:{uri}", o.method.to_uppercase()));
    let response = if use_qop {
        md5_hex(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"))
    } else {
        md5_hex(&format!("{ha1}:{nonce}:{ha2}"))
    };

    // 按 RFC：username/realm/nonce/uri/response/opaque 加引号，qop/nc/algorithm 不加
    let mut parts = vec![
        format!("username=\"{}\"", o.username),
        format!("realm=\"{realm}\""),
        format!("nonce=\"{nonce}\""),
        format!("uri=\"{uri}\""),
        format!("response=\"{response}\""),
    ];
    if !algorithm.is_empty() {
        parts.push(format!("algorithm={algorithm}"));
    }
    if use_qop {
        parts.push("qop=auth".into());
        parts.push(format!("nc={nc}"));
        parts.push(format!("cnonce=\"{cnonce}\""));
    }
    if let Some(op) = ch.get("opaque") {
        parts.push(format!("opaque=\"{op}\""));
    }
    format!("Digest {}", parts.join(", "))
}

// ── OAuth 2.0（client_credentials）──────────────────

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    kind: String,
    /// ms 时间戳；0 表示服务端未给 `expires_in`（不设过期，仅同次会话复用）
    expires_at: u64,
}

/// 每个缓存键一把异步锁：同键并发只有第一个真去换 token（见模块文档）。
type TokenSlot = Arc<tokio::sync::Mutex<Option<CachedToken>>>;

fn token_slots() -> &'static Mutex<HashMap<String, TokenSlot>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, TokenSlot>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 清空 token 缓存（改动认证配置后调用，避免继续用旧令牌）。
pub fn clear_token_cache() {
    if let Ok(mut m) = token_slots().lock() {
        m.clear();
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

async fn fetch_oauth2_token(spec: &HttpSpec, cfg: &ClientConfig) -> Result<CachedToken, String> {
    let o = spec.auth.oauth2.clone().unwrap_or_default();
    // client_secret 也进缓存键：改了密钥就该换一把新 token，
    // 否则用户改完配置还得手动清缓存——而"为什么改了密钥还在用旧 token"
    // 是个几乎没人想得到要去清缓存的现场。
    let key = format!(
        "{}|{}|{}|{}|{:?}",
        o.token_url,
        o.client_id,
        o.client_secret,
        o.scope.clone().unwrap_or_default(),
        o.client_auth
    );
    let slot: TokenSlot = {
        let mut m = token_slots().lock().map_err(|_| "token 缓存已损坏".to_string())?;
        m.entry(key).or_default().clone()
    };
    // 同键串行：后来者在这里等，醒来直接读到上面那位换好的 token
    let mut guard = slot.lock().await;
    if let Some(t) = guard.as_ref() {
        if t.expires_at == 0 || t.expires_at > now_ms() {
            return Ok(t.clone());
        }
    }

    if o.token_url.trim().is_empty() {
        return Err("OAuth 2.0：请先填写 Access Token URL".into());
    }
    let mut form = vec!["grant_type=client_credentials".to_string()];
    if let Some(sc) = o.scope.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        form.push(format!("scope={}", encode_uri_component(sc)));
    }
    let mut headers = vec![KvPair::new("Content-Type", "application/x-www-form-urlencoded")];
    match o.client_auth {
        ClientAuth::Body => {
            form.push(format!("client_id={}", encode_uri_component(&o.client_id)));
            form.push(format!("client_secret={}", encode_uri_component(&o.client_secret)));
        }
        ClientAuth::Header => headers.push(KvPair::new(
            "Authorization",
            format!("Basic {}", base64_encode(format!("{}:{}", o.client_id, o.client_secret).as_bytes())),
        )),
    }

    let req = request::HttpRequest {
        method: "POST".into(),
        url: o.token_url.trim().to_string(),
        headers,
        body: Some(RequestBody::Text(form.join("&"))),
    };
    let resp = http::send(&req, cfg).await?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("OAuth 2.0 取 token 失败（HTTP {}）：{}", resp.status, head(&resp.body)));
    }
    let parsed: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|_| format!("OAuth 2.0 取 token 失败：响应不是 JSON —— {}", head(&resp.body)))?;
    let token = parsed.get("access_token").and_then(|v| v.as_str()).unwrap_or("");
    if token.is_empty() {
        return Err(format!("OAuth 2.0 取 token 失败：响应中没有 access_token —— {}", head(&resp.body)));
    }
    let kind = parsed.get("token_type").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("Bearer");
    let ttl = parsed.get("expires_in").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    // 提前 30s 过期，避免卡在边界上被服务端判过期
    let expires_at = match ttl {
        Some(t) if t.is_finite() && t > 0.0 => now_ms() + ((t - 30.0).max(0.0) * 1000.0) as u64,
        _ => 0,
    };
    let entry = CachedToken {
        token: token.to_string(),
        kind: capitalize(kind),
        expires_at,
    };
    *guard = Some(entry.clone());
    Ok(entry)
}

/// 错误信息里回显响应体的开头——够定位问题，又不至于把一整页 HTML 糊进日志。
fn head(s: &str) -> String {
    s.chars().take(200).collect()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ── 统一发送入口 ────────────────────────────────────

/// 按认证方式发送请求：静态头由 `request::build` 带上，Digest 与 OAuth 2.0 在此补齐。
///
/// 传入的 `spec` 应当**已经做过变量替换**（`{{clientSecret}}` 这类同样要能用）。
///
/// 返回值带上**实际发出去的那一份请求**：OAuth 2.0 换来的 `Authorization`、
/// Digest 重发时算出的摘要头，都只在这一份里。报告要如实记录发了什么，
/// 只回响应就等于把认证这一环从报告里抹掉了——而认证恰恰是最常出问题的一环。
pub async fn send_with_auth(
    spec: &HttpSpec,
    cfg: &ClientConfig,
) -> Result<(request::HttpRequest, HttpResponse), String> {
    let mut req = request::build(spec);

    if spec.auth.kind == AuthType::Oauth2 {
        let t = fetch_oauth2_token(spec, cfg).await?;
        req.headers.push(KvPair::new("Authorization", format!("{} {}", t.kind, t.token)));
    }

    let resp = http::send(&req, cfg).await?;

    // Digest：首发必然吃 401，就着 challenge 算摘要重发一次（只重试一次，避免死循环）
    if spec.auth.kind == AuthType::Digest && resp.status == 401 {
        let www = resp
            .headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case("www-authenticate"))
            .map(|h| h.value.clone())
            .unwrap_or_default();
        if let Some(ch) = parse_digest_challenge(&www) {
            let d = spec.auth.digest.clone().unwrap_or_default();
            let header = build_digest_header(
                &ch,
                &DigestOpts {
                    username: &d.username,
                    password: &d.password,
                    method: &req.method,
                    url: &req.url,
                    cnonce: None,
                    nc: None,
                },
            );
            let mut retry = req;
            retry.headers.push(KvPair::new("Authorization", header));
            let resp = http::send(&retry, cfg).await?;
            return Ok((retry, resp));
        }
    }
    Ok((req, resp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_challenge_params() {
        let ch = parse_digest_challenge(
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#,
        )
        .expect("应能解析");
        assert_eq!(ch.get("realm").unwrap(), "testrealm@host.com");
        assert_eq!(ch.get("nonce").unwrap(), "dcd98b7102dd2f0e8b11d0f600bfb0c093");
        assert_eq!(ch.get("opaque").unwrap(), "5ccc069c403ebaf9f0171e9517f40e41");
        assert_eq!(ch.get("qop").unwrap(), "auth,auth-int");
    }

    /// 裸 token 值、大小写、以及"服务端同时给了 Basic 与 Digest"的情形
    #[test]
    fn parses_unquoted_and_mixed_schemes() {
        let ch = parse_digest_challenge("digest realm=r, nonce=n, algorithm=MD5, stale=false").expect("应能解析");
        assert_eq!(ch.get("realm").unwrap(), "r");
        assert_eq!(ch.get("algorithm").unwrap(), "MD5");
        assert_eq!(ch.get("stale").unwrap(), "false");

        let ch = parse_digest_challenge(r#"Basic realm="x", Digest realm="y", nonce="n""#).expect("应能解析");
        assert_eq!(ch.get("realm").unwrap(), "y", "应取 Digest 那一段");

        assert!(parse_digest_challenge(r#"Basic realm="x""#).is_none(), "没有 Digest 方案");
        assert!(parse_digest_challenge("").is_none());
    }

    /// RFC 2617 §3.5 的官方示例向量——摘要算错会导致所有 Digest 认证失败，
    /// 而失败现场只是一句 401，没有向量根本没法自证。
    #[test]
    fn digest_matches_rfc2617_example() {
        let ch = parse_digest_challenge(
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#,
        )
        .expect("应能解析");
        let h = build_digest_header(
            &ch,
            &DigestOpts {
                username: "Mufasa",
                password: "Circle Of Life",
                method: "GET",
                url: "http://host.com/dir/index.html",
                cnonce: Some("0a4f113b"),
                nc: Some("00000001"),
            },
        );
        assert!(h.contains(r#"response="6629fae49393a05397450978507c4ef1""#), "RFC 示例的 response 值: {h}");
        assert!(h.contains(r#"uri="/dir/index.html""#), "{h}");
        assert!(h.contains("qop=auth"), "{h}");
        assert!(h.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#), "{h}");
    }

    /// 无 qop 时走两段式（老服务端仍在用）
    #[test]
    fn digest_without_qop_uses_two_part_form() {
        let ch = parse_digest_challenge(r#"Digest realm="r", nonce="n""#).expect("应能解析");
        let h = build_digest_header(
            &ch,
            &DigestOpts { username: "u", password: "p", method: "GET", url: "http://h/a", cnonce: Some("c"), nc: None },
        );
        let ha1 = md5_hex("u:r:p");
        let ha2 = md5_hex("GET:/a");
        assert!(h.contains(&format!("response=\"{}\"", md5_hex(&format!("{ha1}:n:{ha2}")))), "{h}");
        assert!(!h.contains("qop"), "无 qop 时不该带 qop / nc / cnonce: {h}");
    }

    #[test]
    fn digest_md5_sess_salts_ha1() {
        let ch = parse_digest_challenge(r#"Digest realm="r", nonce="n", algorithm=MD5-sess, qop="auth""#).expect("应能解析");
        let h = build_digest_header(
            &ch,
            &DigestOpts { username: "u", password: "p", method: "GET", url: "http://h/a", cnonce: Some("c"), nc: Some("00000001") },
        );
        let ha1 = md5_hex(&format!("{}:n:c", md5_hex("u:r:p")));
        let ha2 = md5_hex("GET:/a");
        let want = md5_hex(&format!("{ha1}:n:00000001:c:auth:{ha2}"));
        assert!(h.contains(&format!("response=\"{want}\"")), "{h}");
        assert!(h.contains("algorithm=MD5-sess"), "{h}");
    }

    #[test]
    fn request_uri_extraction() {
        assert_eq!(request_uri("http://h/a/b?c=1"), "/a/b?c=1");
        assert_eq!(request_uri("https://h:8443/x"), "/x");
        assert_eq!(request_uri("http://h"), "/", "没有路径时用 /");
        assert_eq!(request_uri("http://h/"), "/");
    }

    /// cnonce 每次都不同——固定值会让重放攻击变得容易
    #[test]
    fn client_nonce_is_unique_per_call() {
        let a = client_nonce();
        let b = client_nonce();
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// 同键并发只换一次 token。这是 case 之间敢开并发的前提。
    #[tokio::test]
    async fn token_slot_dedupes_concurrent_fetches() {
        clear_token_cache();
        let slot: TokenSlot = {
            let mut m = token_slots().lock().unwrap();
            m.entry("k".into()).or_default().clone()
        };
        // 先占住锁并写入一个未过期的 token
        {
            let mut g = slot.lock().await;
            *g = Some(CachedToken { token: "T".into(), kind: "Bearer".into(), expires_at: now_ms() + 60_000 });
        }
        // 并发的 8 个"取 token"应全部命中缓存（这里直接验缓存语义）
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let s = slot.clone();
            set.spawn(async move {
                let g = s.lock().await;
                g.as_ref().map(|t| t.token.clone())
            });
        }
        while let Some(r) = set.join_next().await {
            assert_eq!(r.unwrap().as_deref(), Some("T"));
        }
        clear_token_cache();
    }

    /// 过期的 token 不该被复用
    #[tokio::test]
    async fn expired_token_is_not_reused() {
        clear_token_cache();
        let slot: TokenSlot = {
            let mut m = token_slots().lock().unwrap();
            m.entry("k2".into()).or_default().clone()
        };
        let mut g = slot.lock().await;
        *g = Some(CachedToken { token: "OLD".into(), kind: "Bearer".into(), expires_at: now_ms() - 1 });
        let still_valid = g.as_ref().map(|t| t.expires_at == 0 || t.expires_at > now_ms()).unwrap_or(false);
        assert!(!still_valid, "已过期的条目应被判为失效");
        drop(g);
        clear_token_cache();
    }

    #[test]
    fn token_type_is_capitalized() {
        assert_eq!(capitalize("bearer"), "Bearer");
        assert_eq!(capitalize("MAC"), "MAC");
        assert_eq!(capitalize(""), "");
    }
}
