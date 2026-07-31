//! 把 `HttpSpec`（用户配置）组装成 `HttpRequest`（真正要发出去的报文）。
//!
//! 这一层是**纯函数**：不碰网络、不读盘、不看时钟。因此"这个 case 到底会发出
//! 什么报文"可以完全靠单测钉死，不需要起服务、不需要抓包。
//!
//! Digest 与 OAuth 2.0 的 `Authorization` **不在这里注入**——前者要先拿到服务端的
//! 401 challenge，后者要先去 token 端点换令牌，都需要发额外请求。它们统一收敛在
//! `auth::send_with_auth`，这里只管静态头（Basic / Bearer / API Key）。

use crate::model::*;
use crate::report::KvPair;

/// 真正要发出去的一份 HTTP 报文。
#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<KvPair>,
    pub body: Option<RequestBody>,
}

/// 请求体三选一。**文件不经内存搬运**：`File` 与 `Form` 里的文件字段
/// 都只带路径，由发送层直接读盘——上传一个 500MB 的包不该先把它塞进一个 String。
#[derive(Debug, Clone, PartialEq)]
pub enum RequestBody {
    Text(String),
    /// binary：以原始字节发送的本地文件路径
    File(String),
    /// multipart/form-data：Content-Type 与 boundary 由发送层生成
    Form(Vec<FormPart>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormPart {
    pub name: String,
    pub value: String,
    /// 有值即文件字段
    pub file_path: Option<String>,
    pub file_name: Option<String>,
    pub content_type: Option<String>,
}

impl HttpRequest {
    /// 报文体的文本形态（报告里记录用）；文件与表单体没有可展示的文本。
    pub fn body_text(&self) -> Option<&str> {
        match &self.body {
            Some(RequestBody::Text(t)) => Some(t),
            _ => None,
        }
    }
}

/// 各请求体类型默认带的 Content-Type。
///
/// 文本类显式带 `charset=utf-8`（对齐新版 Postman）：报文体本就以 UTF-8 字节发送，
/// 声明 charset 可规避 xml / text「无 charset 时默认非 UTF-8」的历史坑，接收端稳解中文。
/// form-data 不在此列——它的 Content-Type 含 boundary，只能由发送层生成。
pub fn default_content_type(t: BodyType) -> Option<&'static str> {
    match t {
        BodyType::Json => Some("application/json; charset=utf-8"),
        BodyType::Xml => Some("application/xml; charset=utf-8"),
        BodyType::Text => Some("text/plain; charset=utf-8"),
        BodyType::FormUrlencoded => Some("application/x-www-form-urlencoded"),
        _ => None,
    }
}

/// 文件扩展名 → MIME（对齐 Postman：binary 选文件后按类型自动定 Content-Type）。
/// binary 与 multipart 的文件字段共用这一张表——两处各维护一份必然会分叉。
const EXT_CONTENT_TYPE: &[(&str, &str)] = &[
    ("json", "application/json"),
    ("xml", "application/xml"),
    ("txt", "text/plain"),
    ("csv", "text/csv"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("css", "text/css"),
    ("js", "text/javascript"),
    ("md", "text/markdown"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tar", "application/x-tar"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("mp4", "video/mp4"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
    ("doc", "application/msword"),
    ("docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
    ("xls", "application/vnd.ms-excel"),
    ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
    ("ppt", "application/vnd.ms-powerpoint"),
    ("pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
];

/// 按扩展名推断 Content-Type，推不出兜底 `application/octet-stream`（同 Postman）。
pub fn guess_content_type(path: &str) -> &'static str {
    let p = path.trim();
    let ext = p
        .rsplit_once('.')
        .map(|(_, e)| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_default();
    EXT_CONTENT_TYPE
        .iter()
        .find(|(k, _)| *k == ext)
        .map(|(_, v)| *v)
        .unwrap_or("application/octet-stream")
}

/// 路径最后一段（multipart 文件 part 的 filename）。两种分隔符都认，跨平台。
pub fn base_name(path: &str) -> String {
    let p = path.trim().trim_end_matches(['/', '\\']);
    p.rsplit(['/', '\\']).next().unwrap_or(p).to_string()
}

// ── URL ↔ query 同步 ────────────────────────────────
//
// 全程**不做百分号编码**：`{{var}}` 里的花括号一编码就废了，而这些值最终
// 由用户自己负责。要编码的场景（form-urlencoded 的报文体）另行处理。

/// 从 url 拆出 base 与 query 数组（保留原样，含 `{{var}}`）。
pub fn split_query_from_url(url: &str) -> (String, Vec<Kv>) {
    let Some(idx) = url.find('?') else {
        return (url.to_string(), Vec::new());
    };
    let base = url[..idx].to_string();
    let mut query = Vec::new();
    for pair in url[idx + 1..].split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.find('=') {
            Some(eq) => (&pair[..eq], &pair[eq + 1..]),
            None => (pair, ""),
        };
        query.push(Kv::new(name, value));
    }
    (base, query)
}

/// 把启用的 query 合并回 url（覆盖 `?` 之后的部分）。
pub fn merge_query_into_url(url: &str, query: &[Kv]) -> String {
    let base = match url.find('?') {
        Some(i) => &url[..i],
        None => url,
    };
    let parts: Vec<String> = query
        .iter()
        .filter(|kv| kv.enabled && (!kv.name.trim().is_empty() || !kv.value.trim().is_empty()))
        .map(|kv| format!("{}={}", kv.name, kv.value))
        .collect();
    if parts.is_empty() {
        return base.to_string();
    }
    format!("{}?{}", base, parts.join("&"))
}

/// 等价于 JS 的 `encodeURIComponent`：不编码 `A-Za-z0-9-_.!~*'()`。
/// 用于 form-urlencoded 报文体——那里的 `&` 与 `=` 是分隔符，不编码就会把字段拆错。
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ── 组装 ────────────────────────────────────────────

/// `HttpSpec` → 待发送报文。传入的 spec 应当**已经做过变量替换**（见 `vars::resolve_http`）。
pub fn build(spec: &HttpSpec) -> HttpRequest {
    // url 里已有的 query 与 query 表格合并——手写 YAML 里两处都可能写
    let (base, in_url) = split_query_from_url(spec.url.trim());
    let mut all_query = in_url;
    all_query.extend(spec.query.iter().cloned());
    let mut url = merge_query_into_url(&base, &all_query);

    let mut headers: Vec<KvPair> = spec
        .headers
        .iter()
        .filter(|h| h.active())
        .map(|h| KvPair::new(h.name.trim(), &h.value))
        .collect();

    apply_static_auth(&spec.auth, &mut headers, &mut url);

    let body = build_body(&spec.body, &mut headers);

    HttpRequest { method: spec.method.trim().to_uppercase(), url, headers, body }
}

/// 静态认证头（Basic / Bearer / API Key）。Digest 与 OAuth 2.0 见模块文档。
fn apply_static_auth(auth: &AuthSpec, headers: &mut Vec<KvPair>, url: &mut String) {
    match auth.kind {
        AuthType::Bearer => {
            if let Some(t) = auth.bearer.as_ref().filter(|b| !b.token.is_empty()) {
                headers.push(KvPair::new("Authorization", format!("Bearer {}", t.token)));
            }
        }
        AuthType::Basic => {
            let b = auth.basic.clone().unwrap_or_default();
            let cred = base64_encode(format!("{}:{}", b.username, b.password).as_bytes());
            headers.push(KvPair::new("Authorization", format!("Basic {cred}")));
        }
        AuthType::Apikey => {
            let Some(k) = auth.apikey.as_ref().filter(|k| !k.key.is_empty()) else {
                return;
            };
            match k.r#in {
                ApikeyIn::Header => headers.push(KvPair::new(&k.key, &k.value)),
                ApikeyIn::Query => {
                    let (_, mut q) = split_query_from_url(url);
                    q.push(Kv::new(&k.key, &k.value));
                    *url = merge_query_into_url(url, &q);
                }
            }
        }
        _ => {}
    }
}

/// UTF-8 安全的 base64（`btoa` 只吃 Latin-1，含中文的用户名 / 密码在前端会直接抛错）。
pub fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn build_body(body: &BodySpec, headers: &mut Vec<KvPair>) -> Option<RequestBody> {
    // 手填的 Content-Type 优先级最高，默认值仅在未手填时补
    let has_ct = |hs: &Vec<KvPair>| hs.iter().any(|h| h.key.eq_ignore_ascii_case("content-type"));
    let set_ct = |hs: &mut Vec<KvPair>, v: &str| {
        if !v.is_empty() && !has_ct(hs) {
            hs.push(KvPair::new("Content-Type", v));
        }
    };

    match body.kind {
        BodyType::Json => {
            let v = body.json.as_ref()?;
            if v.is_null() {
                return None;
            }
            // 缩进 2 空格：报文体在报告与响应区里是给人看的，压成一行没法读
            let text = serde_json::to_string_pretty(v).ok()?;
            if text.trim().is_empty() {
                return None;
            }
            set_ct(headers, default_content_type(BodyType::Json).unwrap_or(""));
            Some(RequestBody::Text(text))
        }
        BodyType::Xml => {
            let t = body.xml.as_deref().filter(|s| !s.trim().is_empty())?;
            set_ct(headers, default_content_type(BodyType::Xml).unwrap_or(""));
            Some(RequestBody::Text(t.to_string()))
        }
        BodyType::Text => {
            let t = body.text.as_deref().filter(|s| !s.is_empty())?;
            let ct = body
                .content_type
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| default_content_type(BodyType::Text).unwrap_or(""));
            set_ct(headers, ct);
            Some(RequestBody::Text(t.to_string()))
        }
        BodyType::FormUrlencoded => {
            let rows = body.urlencoded.as_deref().unwrap_or(&[]);
            let pairs: Vec<String> = rows
                .iter()
                .filter(|k| k.active())
                .map(|k| format!("{}={}", encode_uri_component(k.name.trim()), encode_uri_component(&k.value)))
                .collect();
            if pairs.is_empty() {
                return None;
            }
            set_ct(headers, default_content_type(BodyType::FormUrlencoded).unwrap_or(""));
            Some(RequestBody::Text(pairs.join("&")))
        }
        BodyType::FormData => {
            let rows = body.form_data.as_deref().unwrap_or(&[]);
            let parts: Vec<FormPart> = rows
                .iter()
                // 文件行没选文件（路径空）直接跳过——发下去只会换来一句读盘失败
                .filter(|k| k.active() && (!k.is_file() || !k.value.trim().is_empty()))
                .map(|k| {
                    let name = k.name.trim().to_string();
                    if !k.is_file() {
                        return FormPart { name, value: k.value.clone(), file_path: None, file_name: None, content_type: None };
                    }
                    let path = k.value.trim().to_string();
                    FormPart {
                        name,
                        value: String::new(),
                        file_name: Some(base_name(&path)),
                        content_type: Some(guess_content_type(&path).to_string()),
                        file_path: Some(path),
                    }
                })
                .collect();
            if parts.is_empty() {
                return None;
            }
            // Content-Type 不能手工设：multipart 的 boundary 由发送层生成
            Some(RequestBody::Form(parts))
        }
        BodyType::Binary => {
            let p = body.file_path.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
            // 手填优先，否则按文件类型推断（兜底 octet-stream，故 binary 有文件时总带 Content-Type）
            let ct = body
                .content_type
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| guess_content_type(p));
            set_ct(headers, ct);
            Some(RequestBody::File(p.to_string()))
        }
        BodyType::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> HttpSpec {
        HttpSpec { method: "get".into(), url: "http://x/a".into(), ..Default::default() }
    }

    fn ct(r: &HttpRequest) -> Option<&str> {
        r.headers.iter().find(|h| h.key.eq_ignore_ascii_case("content-type")).map(|h| h.value.as_str())
    }

    #[test]
    fn method_is_normalized_and_headers_filtered() {
        let mut s = spec();
        s.headers = vec![
            Kv::new("X-A", "1"),
            Kv { name: "X-Off".into(), value: "2".into(), enabled: false, description: None },
            Kv::new("   ", "3"),
            Kv::new("  X-Trim  ", "4"),
        ];
        let r = build(&s);
        assert_eq!(r.method, "GET");
        assert_eq!(r.headers, vec![KvPair::new("X-A", "1"), KvPair::new("X-Trim", "4")]);
    }

    /// url 里已有的 query 与 query 表格合并——手写 YAML 时两处都可能写
    #[test]
    fn query_merges_from_url_and_table() {
        let mut s = spec();
        s.url = "http://x/a?u=1".into();
        s.query = vec![
            Kv::new("t", "2"),
            Kv { name: "off".into(), value: "3".into(), enabled: false, description: None },
        ];
        assert_eq!(build(&s).url, "http://x/a?u=1&t=2");

        // 全部禁用时不留下光秃秃的问号
        let mut s2 = spec();
        s2.query = vec![Kv { name: "a".into(), value: "1".into(), enabled: false, description: None }];
        assert_eq!(build(&s2).url, "http://x/a");
    }

    /// 变量占位不做百分号编码——编了就再也替换不回来了
    #[test]
    fn query_values_are_not_percent_encoded() {
        let mut s = spec();
        s.url = "{{base}}/a".into();
        s.query = vec![Kv::new("q", "{{kw}} 空格")];
        assert_eq!(build(&s).url, "{{base}}/a?q={{kw}} 空格");
    }

    #[test]
    fn static_auth_headers() {
        let mut s = spec();
        s.auth = AuthSpec { kind: AuthType::Bearer, bearer: Some(BearerAuth { token: "T".into() }), ..Default::default() };
        assert_eq!(build(&s).headers[0], KvPair::new("Authorization", "Bearer T"));

        // RFC 7617 的官方示例向量
        s.auth = AuthSpec {
            kind: AuthType::Basic,
            basic: Some(BasicAuth { username: "Aladdin".into(), password: "open sesame".into() }),
            ..Default::default()
        };
        assert_eq!(
            build(&s).headers[0],
            KvPair::new("Authorization", "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==")
        );

        // 中文密码：UTF-8 安全的 base64（前端的 btoa 只吃 Latin-1，在这里会直接抛错）
        s.auth = AuthSpec {
            kind: AuthType::Basic,
            basic: Some(BasicAuth { username: "用户".into(), password: "密码".into() }),
            ..Default::default()
        };
        assert_eq!(build(&s).headers[0], KvPair::new("Authorization", "Basic 55So5oi3OuWvhueggQ=="));
    }

    #[test]
    fn apikey_in_header_or_query() {
        let mut s = spec();
        s.auth = AuthSpec {
            kind: AuthType::Apikey,
            apikey: Some(ApikeyAuth { key: "X-Key".into(), value: "V".into(), r#in: ApikeyIn::Header }),
            ..Default::default()
        };
        assert_eq!(build(&s).headers[0], KvPair::new("X-Key", "V"));

        s.auth.apikey.as_mut().unwrap().r#in = ApikeyIn::Query;
        let r = build(&s);
        assert_eq!(r.url, "http://x/a?X-Key=V");
        assert!(r.headers.is_empty(), "放 query 时不该同时加头");

        // 已有 query 时追加而非覆盖
        s.url = "http://x/a?u=1".into();
        assert_eq!(build(&s).url, "http://x/a?u=1&X-Key=V");
    }

    #[test]
    fn json_body_is_pretty_printed_with_default_content_type() {
        let mut s = spec();
        s.body = BodySpec { kind: BodyType::Json, json: Some(json!({ "a": 1, "b": "x" })), ..Default::default() };
        let r = build(&s);
        assert_eq!(r.body_text(), Some("{\n  \"a\": 1,\n  \"b\": \"x\"\n}"));
        assert_eq!(ct(&r), Some("application/json; charset=utf-8"));
    }

    /// 手填的 Content-Type 优先级最高
    #[test]
    fn manual_content_type_wins() {
        let mut s = spec();
        s.headers = vec![Kv::new("Content-Type", "application/vnd.custom+json")];
        s.body = BodySpec { kind: BodyType::Json, json: Some(json!({ "a": 1 })), ..Default::default() };
        assert_eq!(ct(&build(&s)), Some("application/vnd.custom+json"));
    }

    #[test]
    fn form_urlencoded_encodes_values() {
        let mut s = spec();
        s.body = BodySpec {
            kind: BodyType::FormUrlencoded,
            urlencoded: Some(vec![Kv::new("a b", "1&2"), Kv::new("中", "值")]),
            ..Default::default()
        };
        let r = build(&s);
        assert_eq!(r.body_text(), Some("a%20b=1%262&%E4%B8%AD=%E5%80%BC"));
        assert_eq!(ct(&r), Some("application/x-www-form-urlencoded"));
    }

    #[test]
    fn form_data_files_carry_name_and_type() {
        let mut s = spec();
        s.body = BodySpec {
            kind: BodyType::FormData,
            form_data: Some(vec![
                FormItem { name: "t".into(), value: "v".into(), enabled: true, description: None, kind: None },
                FormItem {
                    name: "f".into(),
                    value: "/p/q/a.PNG".into(),
                    enabled: true,
                    description: None,
                    kind: Some(FormKind::File),
                },
                // 文件行没选文件 → 跳过，别换来一句读盘失败
                FormItem { name: "empty".into(), value: "  ".into(), enabled: true, description: None, kind: Some(FormKind::File) },
            ]),
            ..Default::default()
        };
        let r = build(&s);
        let Some(RequestBody::Form(parts)) = &r.body else { panic!("应是 multipart") };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "t");
        assert!(parts[0].file_path.is_none());
        assert_eq!(parts[1].file_name.as_deref(), Some("a.PNG"));
        assert_eq!(parts[1].content_type.as_deref(), Some("image/png"), "扩展名大小写不敏感");
        assert!(ct(&r).is_none(), "multipart 的 Content-Type 由发送层生成（含 boundary）");
    }

    #[test]
    fn binary_body_infers_content_type() {
        let mut s = spec();
        s.body = BodySpec { kind: BodyType::Binary, file_path: Some("/p/a.pdf".into()), ..Default::default() };
        let r = build(&s);
        assert_eq!(r.body, Some(RequestBody::File("/p/a.pdf".into())));
        assert_eq!(ct(&r), Some("application/pdf"));

        // 推不出类型时兜底
        s.body.file_path = Some("/p/noext".into());
        assert_eq!(ct(&build(&s)), Some("application/octet-stream"));
    }

    /// 空报文体一律不发（也不该留下一个 Content-Type 头）
    #[test]
    fn empty_bodies_are_dropped() {
        let variants = [
            BodySpec { kind: BodyType::Json, json: Some(json!(null)), ..Default::default() },
            BodySpec { kind: BodyType::Xml, xml: Some("  ".into()), ..Default::default() },
            BodySpec { kind: BodyType::Text, text: Some(String::new()), ..Default::default() },
            BodySpec { kind: BodyType::FormUrlencoded, urlencoded: Some(vec![]), ..Default::default() },
            BodySpec { kind: BodyType::FormData, form_data: Some(vec![]), ..Default::default() },
            BodySpec { kind: BodyType::Binary, file_path: Some("  ".into()), ..Default::default() },
            BodySpec::default(),
        ];
        for b in variants {
            let mut s = spec();
            let kind = b.kind;
            s.body = b;
            let r = build(&s);
            assert!(r.body.is_none(), "{kind:?} 空体不该发出去");
            assert!(ct(&r).is_none(), "{kind:?} 空体不该留下 Content-Type");
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(base_name("/a/b/c.png"), "c.png");
        assert_eq!(base_name("C:\\a\\b.txt"), "b.txt");
        assert_eq!(base_name("noslash"), "noslash");
        assert_eq!(guess_content_type("a.JSON"), "application/json");
        assert_eq!(guess_content_type("a.unknown"), "application/octet-stream");
        assert_eq!(encode_uri_component("a-_.!~*'()b"), "a-_.!~*'()b", "这些字符不编码");
        let (b, q) = split_query_from_url("http://x?a=1&b&c=2");
        assert_eq!(b, "http://x");
        assert_eq!(q.len(), 3);
        assert_eq!(q[1].name, "b");
        assert_eq!(q[1].value, "");
    }
}
