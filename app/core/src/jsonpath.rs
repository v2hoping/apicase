//! JSONPath 的常用子集：`$` / `.key` / `[n]` / `['key']` / `["key"]`。
//!
//! 刻意**不引入完整 JSONPath 实现**。输出提取与断言里用到的形态就这几种，
//! 而完整语法（过滤器 `?()`、递归下降 `..`、切片、通配）会带来两样东西：
//! 一个几千行的依赖，以及一堆"写得出但说不清结果"的表达式。
//! 需求真的出现时再扩，比一上来就给全套要好收场。
//!
//! 手写扫描而非正则：这条路径在批量运行里对每个 step 的每条断言都会走一遍，
//! 而它要做的只是切分标识符与下标——正则引擎在这里是纯开销。

use serde_json::Value;

/// 按路径取值；路径无效或指向不存在的位置返回 `None`。
///
/// 前导 `$` 可有可无，`data.token` 与 `$.data.token` 等价——
/// 断言栏里手写路径时省掉 `$.` 是很自然的事。
pub fn get<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let s = path.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    if s.is_empty() {
        return Some(root);
    }
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut cur = root;
    // 允许省略前导点：`data.token` 等价于 `.data.token`
    let mut implicit_key = bytes[0] != b'.' && bytes[0] != b'[';

    loop {
        if i >= bytes.len() {
            return Some(cur);
        }
        let seg = if implicit_key {
            implicit_key = false;
            read_ident(s, &mut i)?
        } else {
            match bytes[i] {
                b'.' => {
                    i += 1;
                    read_ident(s, &mut i)?
                }
                b'[' => {
                    i += 1;
                    read_bracket(s, &mut i)?
                }
                // 认不出的片段 → 整条路径视为无效（而不是悄悄按前缀取值）
                _ => return None,
            }
        };
        cur = match seg {
            Seg::Key(k) => cur.as_object()?.get(k.as_ref())?,
            Seg::Index(n) => cur.as_array()?.get(n)?,
        };
    }
}

enum Seg<'a> {
    Key(std::borrow::Cow<'a, str>),
    Index(usize),
}

/// `.key` 里的标识符：字母 / `_` / `$` 开头，后接字母数字 / `_` / `$`。
fn read_ident<'a>(s: &'a str, i: &mut usize) -> Option<Seg<'a>> {
    let b = s.as_bytes();
    let start = *i;
    if start >= b.len() {
        return None;
    }
    let first = b[start];
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    let mut j = start + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$') {
        j += 1;
    }
    *i = j;
    Some(Seg::Key(std::borrow::Cow::Borrowed(&s[start..j])))
}

/// `[0]` / `['key']` / `["key"]`。引号形式支持任意 key（含点、空格、中文）。
fn read_bracket<'a>(s: &'a str, i: &mut usize) -> Option<Seg<'a>> {
    let b = s.as_bytes();
    if *i >= b.len() {
        return None;
    }
    let quote = b[*i];
    if quote == b'\'' || quote == b'"' {
        let start = *i + 1;
        let mut j = start;
        while j < b.len() && b[j] != quote {
            j += 1;
        }
        if j >= b.len() || j + 1 >= b.len() || b[j + 1] != b']' {
            return None;
        }
        *i = j + 2;
        return Some(Seg::Key(std::borrow::Cow::Borrowed(&s[start..j])));
    }
    let start = *i;
    let mut j = start;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    if j == start || j >= b.len() || b[j] != b']' {
        return None;
    }
    *i = j + 1;
    s[start..j].parse::<usize>().ok().map(Seg::Index)
}

/// 宽松解析 JSON：不是 JSON 就返回 `None`（响应体常常是 HTML 或纯文本）。
pub fn parse_json(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "data": { "token": "abc", "list": [10, 20, { "deep": true }], "空 键": 1, "a.b": 2 },
            "n": 0,
            "arr": [],
            "nil": null
        })
    }

    #[test]
    fn basic_paths() {
        let d = doc();
        assert_eq!(get(&d, "$.data.token"), Some(&json!("abc")));
        assert_eq!(get(&d, "data.token"), Some(&json!("abc")), "前导 $. 可省");
        assert_eq!(get(&d, "$.data.list[1]"), Some(&json!(20)));
        assert_eq!(get(&d, "$.data.list[2].deep"), Some(&json!(true)));
        assert_eq!(get(&d, "$"), Some(&d), "单独的 $ 取根");
        assert_eq!(get(&d, "  $.n  "), Some(&json!(0)), "两端空白应忽略");
    }

    /// 带引号的下标能取到点号 / 空格 / 中文这类标识符形式取不到的 key
    #[test]
    fn quoted_keys() {
        let d = doc();
        assert_eq!(get(&d, "$.data['空 键']"), Some(&json!(1)));
        assert_eq!(get(&d, "$.data[\"a.b\"]"), Some(&json!(2)));
        assert_eq!(get(&d, "$.data.a.b"), None, "不加引号时 a.b 是两层，取不到");
    }

    /// null 是"取到了一个 null 值"，与"路径不存在"必须分开——
    /// `exists` 断言全靠这个区分。
    #[test]
    fn null_value_vs_missing() {
        let d = doc();
        assert_eq!(get(&d, "$.nil"), Some(&json!(null)));
        assert_eq!(get(&d, "$.nope"), None);
        assert_eq!(get(&d, "$.nil.x"), None, "在 null 上继续取路径是不存在");
    }

    #[test]
    fn invalid_paths_return_none() {
        let d = doc();
        for p in ["$.data.list[", "$.data.list[a]", "$.data['未闭合", "$..data", "$.data..token", "$.[0]", "$.data list"] {
            assert_eq!(get(&d, p), None, "{p} 应判为无效");
        }
    }

    #[test]
    fn out_of_range_and_type_mismatch() {
        let d = doc();
        assert_eq!(get(&d, "$.data.list[9]"), None, "越界");
        assert_eq!(get(&d, "$.arr[0]"), None, "空数组");
        assert_eq!(get(&d, "$.n[0]"), None, "在数字上取下标");
        assert_eq!(get(&d, "$.data.list.token"), None, "在数组上取 key");
    }

    #[test]
    fn non_json_body_yields_none() {
        assert!(parse_json("<html></html>").is_none());
        assert!(parse_json("").is_none());
        assert_eq!(parse_json("{\"a\":1}"), Some(json!({"a":1})));
    }
}
