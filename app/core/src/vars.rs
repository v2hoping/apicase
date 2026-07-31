//! `{{变量}}` 透传：把 environment、case 级 `vars`、以及上游 step 的 `outputs`
//! 代入请求报文的每一个字段。
//!
//! 两条规则值得记住：
//!
//! - **未解析的表达式保留字面量**。`${{token}}` 查不到时原样发出去，而不是替换成空串。
//!   替成空会得到一个"请求发出去了、服务端说参数不对"的现场，比原样保留难查十倍——
//!   看到 URL 里赫然写着 `{{token}}`，问题一眼可见。
//! - **替换在结构上做，不在文本上做**。JSON 报文体按 `Value` 逐节点替换（key 也替），
//!   因此不存在"变量值里有引号把 JSON 打碎"这类文本拼接的老问题。

use crate::model::*;
use crate::util::js_string;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};

/// 运行期变量上下文：case 级 vars（已并入 environment）+ 各 step 已提取的 outputs。
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    pub vars: BTreeMap<String, Value>,
    /// stepId → { outputName: value }
    pub steps: HashMap<String, BTreeMap<String, Value>>,
}

impl RunContext {
    /// environment 变量打底，case 级 `vars` 覆盖之（case-local 更具体）。
    pub fn new(env: &BTreeMap<String, String>, case_vars: Option<&Map<String, Value>>) -> Self {
        let mut vars: BTreeMap<String, Value> =
            env.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        if let Some(cv) = case_vars {
            for (k, v) in cv {
                vars.insert(k.clone(), v.clone());
            }
        }
        Self { vars, steps: HashMap::new() }
    }

    fn lookup(&self, expr: &str) -> Option<&Value> {
        // `steps.<id>.outputs.<name>`（`requests.` 是早期前缀，一并认）
        for prefix in ["steps.", "requests."] {
            if let Some(rest) = expr.strip_prefix(prefix) {
                let (id, tail) = rest.split_once('.')?;
                let name = tail.strip_prefix("outputs.")?;
                return self.steps.get(id)?.get(name);
            }
        }
        let key = expr.strip_prefix("vars.").unwrap_or(expr);
        self.vars.get(key)
    }
}

/// 替换字符串内所有 `${{ expr }}`。查不到的原样保留（见模块文档）。
///
/// **只认 `${{ }}`**。带上 `$` 是因为 `{` 是 YAML 的流式映射起始指示符：以 `{{` 开头的值
/// 必须整行加引号（`url: '{{baseUrl}}/get'`，不加是语法错；`url: {{host}}` 更会被
/// **静默**读成嵌套映射、URL 直接没了）。而 `$` 不在指示符表里，`url: ${{baseUrl}}/get`
/// 可以裸写。GitHub Actions 用 `${{ }}` 多半也是同一个理由。
///
/// 不兼容无 `$` 的旧写法：留着它等于让同一件事有两种写法，而其中一种会把用户逼回加引号。
pub fn resolve_string(s: &str, ctx: &RunContext) -> String {
    if !s.contains("${{") {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' && i + 2 < b.len() && b[i + 1] == b'{' && b[i + 2] == b'{' {
            // 表达式内不允许出现 `}`——`${{a}b}}` 整体不构成一次替换
            if let Some(end) = find_close(b, i + 3) {
                let expr = s[i + 3..end].trim();
                match ctx.lookup(expr) {
                    // null 与查不到同样保留字面量：写了 `${{token}}` 却拿到 null，
                    // 和根本没这个变量一样都是配置问题，得让人看见
                    Some(v) if !v.is_null() => out.push_str(&js_string(v)),
                    _ => out.push_str(&s[i..end + 2]),
                }
                i = end + 2;
                continue;
            }
        }
        // 按字符前进，避免把多字节 UTF-8 切开
        let ch_len = utf8_len(b[i]);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// 从 `from` 起找 `}}`，中途遇到单个 `}` 即判定不成立（对齐既有的 `[^}]+?` 语义）。
fn find_close(b: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < b.len() {
        if b[j] == b'}' {
            return if j + 1 < b.len() && b[j + 1] == b'}' && j > from { Some(j) } else { None };
        }
        j += 1;
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

fn resolve_kv(list: &[Kv], ctx: &RunContext) -> Vec<Kv> {
    list.iter()
        .map(|k| Kv {
            name: resolve_string(&k.name, ctx),
            value: resolve_string(&k.value, ctx),
            enabled: k.enabled,
            description: k.description.clone(),
        })
        .collect()
}

fn resolve_form(list: &[FormItem], ctx: &RunContext) -> Vec<FormItem> {
    list.iter()
        .map(|k| FormItem {
            name: resolve_string(&k.name, ctx),
            // 文件字段的 value 是路径，同样允许 `{{dir}}/a.png` 这样拼
            value: resolve_string(&k.value, ctx),
            enabled: k.enabled,
            description: k.description.clone(),
            kind: k.kind,
        })
        .collect()
}

/// JSON 报文体：逐节点替换字符串（key 也替），结构本身不动。
fn resolve_json(v: &Value, ctx: &RunContext) -> Value {
    match v {
        Value::String(s) => Value::String(resolve_string(s, ctx)),
        Value::Array(a) => Value::Array(a.iter().map(|x| resolve_json(x, ctx)).collect()),
        Value::Object(m) => {
            let mut out = Map::with_capacity(m.len());
            for (k, val) in m {
                out.insert(resolve_string(k, ctx), resolve_json(val, ctx));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn resolve_auth(a: &AuthSpec, ctx: &RunContext) -> AuthSpec {
    let r = |s: &String| resolve_string(s, ctx);
    AuthSpec {
        kind: a.kind,
        bearer: a.bearer.as_ref().map(|b| BearerAuth { token: r(&b.token) }),
        basic: a.basic.as_ref().map(|b| BasicAuth { username: r(&b.username), password: r(&b.password) }),
        apikey: a.apikey.as_ref().map(|k| ApikeyAuth { key: r(&k.key), value: r(&k.value), r#in: k.r#in }),
        digest: a.digest.as_ref().map(|d| DigestAuth { username: r(&d.username), password: r(&d.password) }),
        oauth2: a.oauth2.as_ref().map(|o| Oauth2Auth {
            token_url: r(&o.token_url),
            client_id: r(&o.client_id),
            client_secret: r(&o.client_secret),
            scope: o.scope.as_ref().map(r),
            client_auth: o.client_auth,
        }),
    }
}

fn resolve_body(b: &BodySpec, ctx: &RunContext) -> BodySpec {
    let r = |s: &String| resolve_string(s, ctx);
    BodySpec {
        kind: b.kind,
        json: b.json.as_ref().map(|j| resolve_json(j, ctx)),
        xml: b.xml.as_ref().map(r),
        text: b.text.as_ref().map(r),
        content_type: b.content_type.as_ref().map(r),
        urlencoded: b.urlencoded.as_ref().map(|l| resolve_kv(l, ctx)),
        form_data: b.form_data.as_ref().map(|l| resolve_form(l, ctx)),
        file_path: b.file_path.as_ref().map(r),
    }
}

/// 对整个请求报文做变量替换（发送前的最后一道）。
pub fn resolve_http(h: &HttpSpec, ctx: &RunContext) -> HttpSpec {
    HttpSpec {
        method: h.method.clone(),
        url: resolve_string(&h.url, ctx),
        query: resolve_kv(&h.query, ctx),
        headers: resolve_kv(&h.headers, ctx),
        auth: resolve_auth(&h.auth, ctx),
        body: resolve_body(&h.body, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> RunContext {
        let mut c = RunContext::default();
        c.vars.insert("base".into(), json!("http://x"));
        c.vars.insert("n".into(), json!(5));
        c.vars.insert("obj".into(), json!({ "a": 1 }));
        c.vars.insert("nil".into(), json!(null));
        c.steps.insert("login".into(), [("token".to_string(), json!("T0K"))].into_iter().collect());
        c
    }

    #[test]
    fn substitutes_vars_and_outputs() {
        let c = ctx();
        assert_eq!(resolve_string("${{base}}/api", &c), "http://x/api");
        assert_eq!(resolve_string("${{ base }}/api", &c), "http://x/api", "内部空白忽略");
        assert_eq!(resolve_string("${{vars.base}}", &c), "http://x", "vars. 前缀可选");
        assert_eq!(resolve_string("${{n}}", &c), "5", "非字符串值转文本");
        assert_eq!(resolve_string("${{obj}}", &c), "{\"a\":1}", "结构体走 JSON");
        assert_eq!(resolve_string("Bearer ${{steps.login.outputs.token}}", &c), "Bearer T0K");
        assert_eq!(resolve_string("${{requests.login.outputs.token}}", &c), "T0K", "旧前缀仍认");
        assert_eq!(resolve_string("${{base}}/${{n}}/${{base}}", &c), "http://x/5/http://x", "多处替换");
    }

    /// **只认 `${{ }}`**：无 `$` 的旧写法不再替换，原样发出去。
    ///
    /// 不留兼容是刻意的——同一件事两种写法，而其中一种会把用户逼回给整行加引号
    /// （`{` 是 YAML 的流式映射指示符）。留着它只会让新写的 case 也随手用旧写法。
    #[test]
    fn only_dollar_form_is_substituted() {
        let c = ctx();
        assert_eq!(resolve_string("${{base}}/api", &c), "http://x/api");
        // 无 $ 的旧写法：原样保留，不当变量
        assert_eq!(resolve_string("{{base}}/api", &c), "{{base}}/api");
        assert_eq!(resolve_string("Bearer {{steps.login.outputs.token}}", &c), "Bearer {{steps.login.outputs.token}}");
        // 混在一起时只替换带 $ 的那个
        assert_eq!(resolve_string("${{base}}/{{n}}", &c), "http://x/{{n}}");
    }

    /// 查不到就原样保留——替成空串会得到一个难查十倍的现场
    #[test]
    fn unresolved_expressions_are_kept_verbatim() {
        let c = ctx();
        assert_eq!(resolve_string("${{missing}}", &c), "${{missing}}");
        assert_eq!(resolve_string("${{nil}}", &c), "${{nil}}", "值为 null 同样保留");
        assert_eq!(resolve_string("${{steps.nope.outputs.x}}", &c), "${{steps.nope.outputs.x}}");
        assert_eq!(resolve_string("${{login.outputs.token}}", &c), "${{login.outputs.token}}", "缺 steps. 前缀不认");
    }

    /// 孤立的 `$` 不该被吃掉——JSONPath（`$.args.foo`）里到处是它
    #[test]
    fn lone_dollar_is_untouched() {
        let c = ctx();
        assert_eq!(resolve_string("$.args.foo", &c), "$.args.foo");
        assert_eq!(resolve_string("$", &c), "$");
        assert_eq!(resolve_string("${base}", &c), "${base}", "单层花括号不是变量语法");
        assert_eq!(resolve_string("价格 $100 起", &c), "价格 $100 起");
        // `$` 紧挨着一个合法引用时，两者都要完整保留
        assert_eq!(resolve_string("$${{base}}", &c), "$http://x");
    }

    #[test]
    fn malformed_braces_are_left_alone() {
        let c = ctx();
        for s in ["${{}}", "${{a}b}}", "$ {{base}}", "${{base", "}}base${{", "${{{base}}"] {
            let out = resolve_string(s, &c);
            assert!(!out.is_empty(), "{s} 不该被吃掉");
        }
        assert_eq!(resolve_string("${{a}b}}", &c), "${{a}b}}", "表达式内含右花括号时不构成替换");
        assert_eq!(resolve_string("${{}}", &c), "${{}}", "空表达式不替换");
        assert_eq!(resolve_string("${{ }}", &c), "${{ }}", "只有空白的表达式不替换");
        // `${{{base}}` 整体被读成表达式 `{base`（查不到 → 原样保留）：
        // 多出来的左括号是表达式的一部分，不是嵌套。
        assert_eq!(resolve_string("${{{base}}", &c), "${{{base}}");
    }

    /// 中文等多字节字符不能在扫描中被切开
    #[test]
    fn multibyte_text_survives() {
        let c = ctx();
        assert_eq!(resolve_string("前缀${{base}}后缀：中文", &c), "前缀http://x后缀：中文");
        assert_eq!(resolve_string("没有变量的中文文本", &c), "没有变量的中文文本");
    }

    /// case 级 vars 覆盖 environment（case-local 更具体）
    #[test]
    fn case_vars_override_environment() {
        let env: BTreeMap<String, String> =
            [("a".to_string(), "env".to_string()), ("b".to_string(), "envB".to_string())].into_iter().collect();
        let cv: Map<String, Value> = [("a".to_string(), json!("case"))].into_iter().collect();
        let c = RunContext::new(&env, Some(&cv));
        assert_eq!(resolve_string("${{a}}/${{b}}", &c), "case/envB");
    }

    /// 替换在结构上做：变量值里的引号不会把 JSON 打碎
    #[test]
    fn json_body_is_resolved_structurally() {
        let mut c = ctx();
        c.vars.insert("evil".into(), json!("a\"b"));
        let body = json!({ "${{base}}": "${{evil}}", "list": ["${{n}}", { "k": "${{base}}" }] });
        let out = resolve_json(&body, &c);
        assert_eq!(out, json!({ "http://x": "a\"b", "list": ["5", { "k": "http://x" }] }));
        // 序列化后仍是合法 JSON（文本拼接的做法在这里就碎了）
        assert!(serde_json::from_str::<Value>(&out.to_string()).is_ok());
    }

    #[test]
    fn resolves_whole_request() {
        let c = ctx();
        let h = HttpSpec {
            method: "POST".into(),
            url: "${{base}}/login".into(),
            query: vec![Kv::new("t", "${{steps.login.outputs.token}}")],
            headers: vec![Kv::new("X-${{n}}", "${{base}}")],
            auth: AuthSpec {
                kind: AuthType::Bearer,
                bearer: Some(BearerAuth { token: "${{steps.login.outputs.token}}".into() }),
                ..Default::default()
            },
            body: BodySpec {
                kind: BodyType::FormData,
                form_data: Some(vec![FormItem {
                    name: "f".into(),
                    value: "${{base}}/a.png".into(),
                    enabled: true,
                    description: None,
                    kind: Some(FormKind::File),
                }]),
                ..Default::default()
            },
        };
        let r = resolve_http(&h, &c);
        assert_eq!(r.url, "http://x/login");
        assert_eq!(r.query[0].value, "T0K");
        assert_eq!(r.headers[0].name, "X-5");
        assert_eq!(r.auth.bearer.unwrap().token, "T0K");
        assert_eq!(r.body.form_data.unwrap()[0].value, "http://x/a.png", "文件路径也参与替换");
        assert_eq!(r.method, "POST", "方法不做替换");
    }

    /// 没有 `{{` 的字符串走快路径，原样返回
    #[test]
    fn fast_path_for_plain_strings() {
        let c = ctx();
        let s = "http://plain/url?a=1";
        assert_eq!(resolve_string(s, &c), s);
    }
}
