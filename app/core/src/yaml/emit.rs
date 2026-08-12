//! YAML 输出器 —— case 文件的写盘格式由这里唯一决定。
//!
//! # 为什么自己写而不是用 `serde_yaml::to_string`
//!
//! 落盘的 case 是**用户要读、要 diff、要 review** 的源文件，格式不是实现细节。
//! 通用序列化器给不了两样东西：序列的缩进风格（`serde_yaml` 不缩进序列项，
//! 与仓库里既有的所有 case 文件不一致），以及**加引号的时机**。
//!
//! # 加引号的唯一理由：不加就读不回原值
//!
//! 这条判据取代了此前那套"防止被读成别的类型"的规则，因为后者会加出一堆多余的引号：
//!
//! ```yaml
//! query:
//!   - name: foo
//!     value: bar        # ← 不带引号
//!   - name: page
//!     value: '2'        # ← 带引号，同一列两副面孔
//! ```
//!
//! `Kv.value` 在 schema 里**恒是字符串**，解析侧本来就会把 `2` 转成 `"2"`
//! （见 `yaml::s()`）。也就是说裸写 `value: 2` 读回来仍是 `"2"`——**往返本就等价**，
//! 那个引号纯属多余。同理 `apicase: 0.1`、`value: true`、`target: 2026-07-30` 都可以裸写。
//!
//! 于是判据收敛成两条，缺一不可：
//!
//! 1. **语法安全**：空串、首尾空白、结构指示符开头、含 `: ` 或 ` #`、含控制字符——
//!    这些不加引号就是语法错误或读成别的结构，与类型无关。
//! 2. **往返等价**：裸写之后用**真实解析器**读回来、按解析侧的规则转成字符串，
//!    还是不是原值。不是才加引号。
//!
//! 实测下来只有七类会栽在第 2 条：`1.10`→`1.1`（尾零）、`1e5`→`100000.0`、
//! `0x1f`→`31`、`+5`→`5`、`True`→`true`、`null` / `~`→空串。**其余一律裸写。**
//!
//! 用真实解析器而不是手写一套判定表，是因为"什么算数字"这件事的边界
//! （`1_000` 是字符串、`007` 是字符串、`.inf` 是数字）只有解析器自己说了算，
//! 手写的表迟早会和它分叉。
//!
//! # 两种位置
//!
//! | 位置 | 规则 | 出现在 |
//! |---|---|---|
//! | `Pos::Str` | 语法安全 + 往返等价 | key，以及 schema 固定为字符串的字段值 |
//! | `Pos::Free` | 语法安全 + **类型必须保住** | `vars` 与 `body.json` 的子树 |
//!
//! `Free` 区域里类型是用户自由决定的：`retries: 3` 与 `retries: '3'` 语义不同，
//! 故凡是能被解析成非字符串的一律加引号。

use serde_json::Value;
use std::fmt::Write as _;

/// 加引号的严格程度（见模块文档）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    /// **严格**：字符串必须"看起来是字符串"。用于会发到网络上的报文内容
    /// （`query` / `headers` / 表单的值）——HTTP 里没有数字类型，`value: 2`
    /// 裸写虽然也读得回 `"2"`，但读的人无从判断类型。
    Strict,
    /// **宽松**：读得回原值就裸写。用于 key（永远按字符串取用、无类型歧义），
    /// 以及 `assertions` 的期望值（不进报文、类型跟着 `target` 走）。
    Loose,
}

/// 子树切到「宽松」的 key。
///
/// 只有 `assertions`：它的 `value` 是**期望值**而不是报文内容，类型该跟着 `target` 走
/// （`status` 就是数字，写 `value: 200` 才自然）。而且断言的比较本就是宽松的
/// （先按文本、再按数字试一遍，见 `assert::loose_eq`），引号对结果毫无影响——
/// 强加一个 `'200'` 纯粹是噪声。
///
/// `query` / `headers` / 表单的 `value` 不在此列：那些会发到网络上，是真的字符串。
const LOOSE_SUBTREE_KEYS: &[&str] = &["assertions"];

/// 把一个 JSON 值输出为 YAML 文本（末尾带换行）。
///
/// 顶层若是空容器或标量，也能正确输出（`{}` / `[]` / `null`），
/// 尽管 case 文件的顶层实际总是一个非空映射。
pub fn to_yaml(v: &Value) -> String {
    let mut out = String::with_capacity(1024);
    match v {
        Value::Object(m) if !m.is_empty() => emit_map(m, 0, Quote::Strict, &mut out),
        Value::Array(a) if !a.is_empty() => emit_seq(a, 0, Quote::Strict, &mut out),
        other => {
            out.push_str(&scalar(other, Quote::Strict));
            out.push('\n');
        }
    }
    out
}

/// 把一个字符串输出成能直接拼进 YAML 的行内标量（不带换行）。
///
/// 用**宽松**规则，理由同 `emit_pair` 里的 key：调用方（顶层 `active`）的值永远按
/// 字符串取用，不存在类型歧义，`dev` 不该被写成 `'dev'`。名字里有冒号、`#` 之类时
/// 仍会正确加上引号。
pub fn inline_scalar(s: &str) -> String {
    scalar_str(s, Quote::Loose)
}

fn indent_to(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push(' ');
    }
}

/// 写一对 `key:` 与它的值。映射与**序列项**共用同一份规则——序列项要把第一个 key
/// 紧跟在 `- ` 后面，于是它有自己的 key 循环；规则各写一遍迟早只改一处而分叉。
fn emit_pair(k: &str, v: &Value, indent: usize, q: Quote, out: &mut String) {
    // key 恒用宽松规则：它永远按字符串取用，不存在类型歧义
    out.push_str(&scalar_str(k, Quote::Loose));
    out.push(':');
    // 一旦进入宽松子树就整棵宽松，不再回退
    let child = if q == Quote::Loose || LOOSE_SUBTREE_KEYS.contains(&k) { Quote::Loose } else { q };
    emit_after_key(v, indent, child, out);
}

fn emit_map(m: &serde_json::Map<String, Value>, indent: usize, q: Quote, out: &mut String) {
    for (k, v) in m {
        indent_to(out, indent);
        emit_pair(k, v, indent, q, out);
    }
}

/// 写 `key:` 之后的部分：非空容器换行 + 缩进，标量同行。
fn emit_after_key(v: &Value, indent: usize, q: Quote, out: &mut String) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            out.push('\n');
            emit_map(m, indent + 2, q, out);
        }
        Value::Array(a) if !a.is_empty() => {
            // 序列项缩进一级（`steps:` 下面是 `  - id: …`）。
            // 两种缩进在 YAML 里都合法，选这种只因为仓库里既有的 case 文件都是这样——
            // 换一种会让所有文件在首次保存时产生无意义的全文 diff。
            out.push('\n');
            emit_seq(a, indent + 2, q, out);
        }
        Value::String(s) => {
            emit_string_after_marker(s, indent, q, out);
        }
        other => {
            out.push(' ');
            out.push_str(&scalar(other, q));
            out.push('\n');
        }
    }
}

fn emit_seq(a: &[Value], indent: usize, q: Quote, out: &mut String) {
    for item in a {
        indent_to(out, indent);
        out.push('-');
        match item {
            Value::Object(m) if !m.is_empty() => {
                // 第一个 key 紧跟在 `- ` 后面，其余 key 与之左对齐
                out.push(' ');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        indent_to(out, indent + 2);
                    }
                    emit_pair(k, v, indent + 2, q, out);
                }
            }
            Value::Array(inner) if !inner.is_empty() => {
                out.push('\n');
                emit_seq(inner, indent + 2, q, out);
            }
            Value::String(s) => emit_string_after_marker(s, indent, q, out),
            other => {
                out.push(' ');
                out.push_str(&scalar(other, q));
                out.push('\n');
            }
        }
    }
}

/// 字符串值：能用块标量（`|`）就用——多行文本（`docs:` 的 markdown、XML 报文体）
/// 写成一行带 `\n` 转义的双引号串没法看，更没法 review。
fn emit_string_after_marker(s: &str, indent: usize, q: Quote, out: &mut String) {
    if let Some(header) = block_header(s) {
        out.push(' ');
        out.push_str(header);
        out.push('\n');
        for line in s.trim_end_matches('\n').split('\n') {
            if line.is_empty() {
                out.push('\n'); // 空行不缩进，免得留下一串尾随空格
            } else {
                indent_to(out, indent + 2);
                out.push_str(line);
                out.push('\n');
            }
        }
        return;
    }
    out.push(' ');
    out.push_str(&scalar_str(s, q));
    out.push('\n');
}

/// 该用块标量吗？能则返回块头（`|` / `|-`）。
///
/// 三条排除是块标量本身表达不了的：`\r` 与控制字符会被规范化掉、
/// 行尾空格在块里会丢失、首行缩进需要显式指示符（`|2-`，可读性反而更差）。
/// 排除掉就退回引号形式，宁可难看也不能改内容。
fn block_header(s: &str) -> Option<&'static str> {
    if !s.contains('\n') {
        return None;
    }
    if s.chars().any(|c| c == '\r' || (c.is_control() && c != '\n')) {
        return None;
    }
    let body = s.trim_end_matches('\n');
    if body.is_empty() {
        return None;
    }
    if body.split('\n').any(|l| l.ends_with(' ') || l.ends_with('\t')) {
        return None;
    }
    if body.starts_with(' ') || body.starts_with('\t') {
        return None;
    }
    match s.len() - body.len() {
        0 => Some("|-"),      // strip：内容不以换行结尾
        1 => Some("|"),       // clip：正好一个尾换行（文本文件的常态）
        _ => None,            // keep（`|+`）：多个尾换行，交给引号形式，别为罕见情形加复杂度
    }
}

fn scalar(v: &Value, q: Quote) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number(n),
        Value::String(s) => scalar_str(s, q),
        // 空容器走流式写法；非空的在上游已分流，不会走到这里
        Value::Array(_) => "[]".into(),
        Value::Object(_) => "{}".into(),
    }
}

/// 浮点必须保住小数点：`1.0` 输出成 `1` 会在读回时变成整数，类型悄悄漂移。
fn number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    match n.as_f64() {
        Some(f) if f.is_finite() => {
            let s = f.to_string();
            if s.contains('.') || s.contains('e') || s.contains('E') {
                s
            } else {
                format!("{s}.0")
            }
        }
        // 非有限值在 JSON 里本来就表示不了，兜底成 null 而不是写出非法 YAML
        _ => "null".into(),
    }
}

fn scalar_str(s: &str, q: Quote) -> String {
    if needs_double_quote(s) {
        return double_quoted(s);
    }
    if needs_quote(s, q) {
        return single_quoted(s);
    }
    s.to_string()
}

/// 单引号也表达不了的：换行与控制字符（单引号串里它们没有转义语法）。
fn needs_double_quote(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

fn single_quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// YAML 的行内指示符：出现在标量**首字符**时会改变结构语义。
const LEADING_INDICATORS: &[char] =
    &[',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`'];

fn needs_quote(s: &str, q: Quote) -> bool {
    if syntactically_unsafe(s) {
        return true;
    }
    match q {
        Quote::Strict => !parses_as_string(s),
        Quote::Loose => !plain_round_trips(s),
    }
}

/// 不加引号就是语法错误、或会被读成别的**结构**——与类型无关，两种位置都适用。
fn syntactically_unsafe(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // 首尾空白裸写会被 YAML 剥掉
    if s.trim() != s {
        return true;
    }
    let first = s.chars().next().unwrap();
    if LEADING_INDICATORS.contains(&first) {
        return true;
    }
    // `-` / `?` / `:` 只在「单独出现或后跟空格」时才是指示符：
    // `-70`、`?查询` 可以裸写，`- ` 与 `?` 不行。
    if matches!(first, '-' | '?' | ':') {
        let rest = &s[first.len_utf8()..];
        if rest.is_empty() || rest.starts_with(' ') {
            return true;
        }
    }
    // `: ` 会被读成 key 分隔符，` #` 会被读成行内注释起始
    s.contains(": ") || s.contains(" #") || s.ends_with(':')
}

/// 裸写它会被读成字符串吗？（`Strict` 用）
///
/// **用真实解析器判定**，不手写判定表：「什么算数字」的边界（`1_000` 是字符串、
/// `007` 是字符串、`.inf` 是数字）只有解析器自己说了算，手写的表迟早会和它分叉。
fn parses_as_string(s: &str) -> bool {
    if !maybe_typed_literal(s) {
        return true;
    }
    matches!(serde_yaml::from_str::<serde_yaml::Value>(s), Ok(serde_yaml::Value::String(v)) if v == s)
}

/// 裸写它，再按解析侧的规则读回来，还是它自己吗？（`Loose` 用）
fn plain_round_trips(s: &str) -> bool {
    if !maybe_typed_literal(s) {
        return true;
    }
    match serde_yaml::from_str::<serde_yaml::Value>(s) {
        Ok(v) => scalar_text(&v).as_deref() == Some(s),
        // 解析不了说明裸写本就不合法（语法检查该拦住的漏网之鱼）
        Err(_) => false,
    }
}

/// 解析侧把标量转字符串的规则（与 `yaml::s()` 一致）；非标量返回 None。
fn scalar_text(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::Null => Some(String::new()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// 廉价预判：这串**有可能**被读成数字 / 布尔 / null 吗？
///
/// 宁可多判几个（多花一次解析，无害），也不能漏判（漏了就是类型悄悄变了）。
/// 数字一定以数字或 `+` / `-` / `.` 开头；布尔与 null 就那几个关键字。
fn maybe_typed_literal(s: &str) -> bool {
    let Some(c) = s.chars().next() else { return false };
    c.is_ascii_digit()
        || matches!(c, '+' | '-' | '.' | '~')
        || matches!(s, "true" | "True" | "TRUE" | "false" | "False" | "FALSE" | "null" | "Null" | "NULL")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_scalars_stay_bare() {
        for s in ["bar", "http://x/y", "GET", "$.data.token", "step1", "a-b_c", "中文值", "-abc", "v1.2.3", "yes", "2026-07-30"] {
            assert_eq!(scalar_str(s, Quote::Strict), s, "{s} 不该被加引号");
        }
    }

    /// **值位置：字符串就得看起来是字符串。**
    ///
    /// `value: 2` 裸写虽然也读得回 `"2"`，但看的人无从判断它是数字还是字符串——
    /// 而 query / header / 表单的值语义上恒是字符串（HTTP 报文里就没有数字类型）。
    /// 这也是 Postman / Insomnia 那边的形态（它们的 collection 是 JSON，值恒是 `"2"`）。
    #[test]
    fn string_values_that_look_typed_get_quoted() {
        for s in ["2", "200", "0.1", "-70", "1.10", "1e5", "0x1f", "+5", "true", "false", "True", "null", "~"] {
            let out = scalar_str(s, Quote::Strict);
            assert!(out.starts_with('\''), "{s} 在值位置应加引号，实际 {out}");
        }
        // 本就是字符串的一律裸写——`yes` / 日期 / 下划线数字在 YAML 1.2 里都是字符串
        for s in ["yes", "no", "y", "n", "on", "off", "2026-07-30", "12:30", "007", "1_000", "v1", "0.1.0"] {
            assert_eq!(scalar_str(s, Quote::Strict), s, "{s} 本就读成字符串，不该加引号");
        }
    }

    /// `assertions` 子树用**宽松**规则：期望值不进报文、类型跟着 `target` 走
    /// （`status` 就是数字），且断言比较本就宽松，引号对结果毫无影响。
    /// 而 `query` / `headers` 的值会发到网络上，仍用严格规则。
    #[test]
    fn assertion_values_are_bare_but_payload_values_are_quoted() {
        let v = json!({
            "steps": [{
                "request": { "query": [{ "name": "page", "value": "2" }] },
                "assertions": [
                    { "target": "status", "op": "eq", "value": "200" },
                    { "target": "$.args.page", "op": "eq", "value": "2" },
                    { "target": "$.ok", "op": "eq", "value": "true" },
                    { "target": "$.msg", "op": "eq", "value": "ok" }
                ]
            }]
        });
        let out = to_yaml(&v);
        // 报文里的值：带引号
        assert!(out.contains("          value: '2'\n"), "query 的值该带引号：\n{out}");
        // 断言的期望值：裸写
        for want in ["        value: 200\n", "        value: 2\n", "        value: true\n", "        value: ok\n"] {
            assert!(out.contains(want), "断言期望值该裸写 {want:?}：\n{out}");
        }
    }

    /// 宽松只是"不为类型加引号"，语法必需与往返有损照样加
    #[test]
    fn loose_still_quotes_when_it_must() {
        assert_eq!(scalar_str("1.10", Quote::Loose), "'1.10'", "往返有损");
        assert_eq!(scalar_str("", Quote::Loose), "''", "空串");
        assert_eq!(scalar_str("{{x}}", Quote::Loose), "'{{x}}'", "指示符开头");
        assert_eq!(scalar_str("a: b", Quote::Loose), "'a: b'", "含 key 分隔符");
    }

    /// **key 位置：只要读得回原值就裸写。**
    /// key 永远按字符串取用，不存在"是数字还是字符串"的歧义——画布坐标的 `y:` 正是靠这条。
    #[test]
    fn keys_only_need_round_trip() {
        for s in ["y", "n", "yes", "no", "on", "off", "x", "2", "200", "-70", "0.1", "true"] {
            assert_eq!(scalar_str(s, Quote::Loose), s, "key {s} 不该加引号");
        }
        // 读回来变了样的仍要引号
        for s in ["1.10", "1e5", "0x1f", "+5", "True", "null", "~"] {
            assert!(scalar_str(s, Quote::Loose).starts_with('\''), "key {s} 应加引号");
        }
    }

    /// 判定用**真实解析器**，不手写判定表——"什么算数字"的边界
    /// （`1_000` 是字符串、`007` 是字符串、`.inf` 是数字）只有解析器说了算。
    #[test]
    fn quote_check_matches_the_real_parser() {
        for s in ["yes", "007", "1_000", "bar", "2026-07-30"] {
            assert!(parses_as_string(s), "{s} 应判为字符串");
        }
        for s in ["2", "0.1", "true", "null", ".inf"] {
            assert!(!parses_as_string(s), "{s} 应判为非字符串");
        }
        // 廉价预判不能漏判（漏了就是类型悄悄变了），多判几个只是多花一次解析
        for s in ["2", "1.10", "1e5", "0x1f", "+5", "True", "null", "~", "0.1", "-70"] {
            assert!(maybe_typed_literal(s), "{s} 必须进入验证");
        }
        for s in ["bar", "http://x/y", "$.args.foo", "GET", "yes"] {
            assert!(!maybe_typed_literal(s), "{s} 可走快速路径");
        }
    }

    /// 单引号 vs 双引号：YAML 两种都合法但**语义不同**——
    /// 单引号是字面量（唯一的转义是把 `'` 写两遍，**反斜杠原样**），
    /// 双引号是唯一能表达任意字符串的风格（支持 `\n` / `\t` / `\uXXXX`）。
    ///
    /// 故默认单引号、只在单引号表达不了时才升级到双引号：apicase 里要加引号的值
    /// 常含反斜杠（断言的 `matches` 正则、Windows 文件路径），双引号会逼着把
    /// `\d` 写成 `\\d`——凭空多一层转义，读的人还得在脑子里反向解一遍。
    #[test]
    fn single_quotes_by_default_double_only_when_needed() {
        // 需要引号且含反斜杠：单引号下反斜杠原样，不必双写
        assert_eq!(scalar_str(r"{2,3}\d+", Quote::Strict), r"'{2,3}\d+'");
        assert_eq!(scalar_str(r" \d+", Quote::Strict), r"' \d+'", "首空格要引号，反斜杠仍原样");
        // 单引号表达不了控制字符 → 升级双引号并转义
        assert_eq!(scalar_str("a\tb", Quote::Strict), "\"a\\tb\"");
        assert_eq!(scalar_str("a\u{1}b", Quote::Strict), "\"a\\u0001b\"");
        // 内容里的单引号按 YAML 规则翻倍（而不是为它切到双引号）
        assert_eq!(scalar_str("'quoted'", Quote::Strict), "'''quoted'''");
        // 大多数含引号 / 反斜杠的值根本不需要引号
        for s in [r"^\d+$", r"C:\Users\a", "it's ok", "say \"hi\""] {
            assert_eq!(scalar_str(s, Quote::Strict), s, "{s} 不该被加引号");
        }
    }

    /// 结构指示符**只在首字符时**才是指示符。`{` 尤其要分清：
    /// `{{baseUrl}}/get` 裸写是语法错（更糟的是 `{{host}}` 会被静默读成嵌套映射，
    /// URL 就没了），而 `http://x/{{id}}` 完全合法——**别为了省事一律加引号**，
    /// apicase 的 URL 里到处是 `{{var}}`，多加的每个引号都是噪声。
    #[test]
    fn braces_only_matter_at_the_start() {
        assert_eq!(scalar_str("{{baseUrl}}/get", Quote::Strict), "'{{baseUrl}}/get'");
        assert_eq!(scalar_str("{{host}}", Quote::Strict), "'{{host}}'");
        // 中间的大括号不影响，裸写即可
        for s in ["http://x/{{id}}", "api/{{v}}/users", "a{b}c", "x{{b}}"] {
            assert_eq!(scalar_str(s, Quote::Strict), s, "{s} 的大括号不在首位，不该加引号");
        }
        // 其余指示符同理：只看首字符
        assert_eq!(scalar_str("[a]", Quote::Strict), "'[a]'");
        assert_eq!(scalar_str("a[0]", Quote::Strict), "a[0]", "JSONPath 下标不该被加引号");
        assert_eq!(scalar_str("#tag", Quote::Strict), "'#tag'");
        assert_eq!(scalar_str("C#", Quote::Strict), "C#", "井号不在首位且前面没有空格");
    }

    /// `vars` 与 `body.json` 的子树里类型由用户决定，规则更严：
    /// 凡是会被读成非字符串的都要加引号，否则 `retries: '3'` 会变成数字 3。
    #[test]
    fn free_subtree_preserves_types() {
        for s in ["2", "0.1", "-70", "true", "false", "1.10"] {
            let out = scalar_str(s, Quote::Strict);
            assert!(out.starts_with('\''), "{s} 在 vars/json 里必须加引号保住字符串类型，实际 {out}");
        }
        // 本就读成字符串的，即便在 Free 位置也不必加
        for s in ["bar", "yes", "y", "2026-07-30", "007", "1_000"] {
            assert_eq!(scalar_str(s, Quote::Strict), s, "{s} 本就读成字符串");
        }
    }

    /// 往返判定用**真实解析器**，不手写判定表——"什么算数字"的边界
    /// （`1_000` 是字符串、`007` 是字符串、`.inf` 是数字）只有解析器说了算。
    #[test]
    fn round_trip_check_matches_the_real_parser() {
        for s in ["2", "0.1", "-70", "true", "007", "1_000", ".inf", "yes", "bar"] {
            assert!(plain_round_trips(s), "{s} 应判为往返等价");
        }
        for s in ["1.10", "1e5", "0x1f", "+5", "True", "null", "~"] {
            assert!(!plain_round_trips(s), "{s} 应判为往返有损");
        }
        // 廉价预判不能漏判（漏了就是往返丢数据），多判几个只是多花一次解析
        for s in ["1.10", "1e5", "0x1f", "+5", "True", "null", "~", "0.1", "-70"] {
            assert!(maybe_typed_literal(s), "{s} 必须进入验证");
        }
        for s in ["bar", "http://x/y", "$.args.foo", "GET"] {
            assert!(!maybe_typed_literal(s), "{s} 可走快速路径");
        }
    }

    /// YAML 1.1 把 `y`/`n`/`yes`/`no` 当布尔别名，1.2 不再如此。
    /// 我们的解析器按 1.2 读（它们就是字符串），故两个位置都不加引号。
    #[test]
    fn yaml11_bool_aliases_need_no_quotes() {
        for s in ["y", "n", "yes", "no", "on", "off", "Y", "NO"] {
            assert_eq!(scalar_str(s, Quote::Strict), s, "{s} 不该加引号");
            assert_eq!(scalar_str(s, Quote::Loose), s, "key 位置的 {s} 也不必加");
        }
        // 真布尔字面量：值位置要引号（否则读成 bool），key 位置读回来还是 "true" 故裸写
        assert_eq!(scalar_str("true", Quote::Strict), "'true'");
        assert_eq!(scalar_str("true", Quote::Loose), "true");
    }

    /// step 的坐标按普通块式写，一行一个字段——全文件一套写法，不给坐标开小灶。
    /// 顺带钉住 `y` 不带引号（YAML 1.1 把 y 当布尔别名，旧的 js-yaml 会写成 `'y'`）。
    #[test]
    fn step_ui_uses_block_style() {
        let v = json!({ "steps": [{ "id": "a", "ui": { "x": 502, "y": -70 }, "request": { "method": "GET" } }] });
        assert_eq!(
            to_yaml(&v),
            "steps:\n  - id: a\n    ui:\n      x: 502\n      y: -70\n    request:\n      method: GET\n"
        );
    }

    /// 序列项缩进一级，map 项的第一个 key 跟在 `- ` 后
    #[test]
    fn sequence_indentation() {
        let v = json!({ "steps": [ { "id": "get", "protocol": "http" }, { "id": "post" } ] });
        assert_eq!(
            to_yaml(&v),
            "steps:\n  - id: get\n    protocol: http\n  - id: post\n"
        );
    }

    /// 多行文本走块标量：以换行结尾用 `|`，不以换行结尾用 `|-`
    #[test]
    fn multiline_uses_block_scalar() {
        let v = json!({ "docs": "第一行\n第二行\n" });
        assert_eq!(to_yaml(&v), "docs: |\n  第一行\n  第二行\n");

        let v = json!({ "docs": "第一行\n第二行" });
        assert_eq!(to_yaml(&v), "docs: |-\n  第一行\n  第二行\n");

        // 空行不缩进——否则留下一串看不见的尾随空格
        let v = json!({ "docs": "a\n\nb\n" });
        assert_eq!(to_yaml(&v), "docs: |\n  a\n\n  b\n");
    }

    /// 块标量表达不了的内容退回引号，绝不改动内容本身
    #[test]
    fn block_scalar_falls_back_when_unrepresentable() {
        assert_eq!(block_header("行尾有空格 \n第二行\n"), None, "行尾空格在块里会丢");
        assert_eq!(block_header("  首行缩进\n第二行\n"), None, "首行缩进需要显式指示符");
        assert_eq!(block_header("a\r\nb\n"), None, "CRLF 会被规范化");
        assert_eq!(block_header("a\n\n\n"), None, "多个尾换行需要 |+");
        // 退回后仍是完整内容（双引号 + 转义）
        let out = scalar_str("a\r\nb", Quote::Strict);
        assert_eq!(out, "\"a\\r\\nb\"");
    }

    #[test]
    fn floats_keep_their_decimal_point() {
        let v = json!({ "a": 1.0, "b": 1.5, "c": 10 });
        assert_eq!(to_yaml(&v), "a: 1.0\nb: 1.5\nc: 10\n");
    }

    #[test]
    fn empty_containers_use_flow_style() {
        let v = json!({ "vars": {}, "steps": [] });
        assert_eq!(to_yaml(&v), "vars: {}\nsteps: []\n");
    }

    /// 单引号内的单引号按 YAML 规则翻倍
    #[test]
    fn single_quote_escaping() {
        assert_eq!(scalar_str("it's", Quote::Strict), "it's", "普通撇号不触发引号");
        assert_eq!(scalar_str("'quoted'", Quote::Strict), "'''quoted'''");
    }

    /// 结构指示符开头 / 行内注释起始 / key 分隔符都要挡住
    #[test]
    fn structural_indicators_get_quoted() {
        for s in ["- item", "{a}", "[a]", "#comment", "&anchor", "*ref", "!tag", "a: b", "a #b", "key:", "@at", "`tick`"] {
            let out = scalar_str(s, Quote::Strict);
            assert!(out.starts_with('\'') || out.starts_with('"'), "{s} 应加引号，实际 {out}");
        }
        // `{{var}}` 是 apicase 最常见的值形态，必须被引号包住才不会被读成流式映射
        assert_eq!(scalar_str("{{baseUrl}}/get", Quote::Strict), "'{{baseUrl}}/get'");
    }

    #[test]
    fn nested_structures() {
        let v = json!({
            "body": { "type": "json", "json": { "a": 1, "b": [1, 2, { "c": "d" }] } }
        });
        assert_eq!(
            to_yaml(&v),
            "body:\n  type: json\n  json:\n    a: 1\n    b:\n      - 1\n      - 2\n      - c: d\n"
        );
    }
}
