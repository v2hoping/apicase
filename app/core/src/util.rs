//! JSON 值 → 文本 / 数字的转换。
//!
//! 语义**刻意对齐 JavaScript 的 `String()` 与 `Number()`**：断言的宽松比较
//! （`status eq 200` 要能匹配数字 200 与字符串 "200"）此前由前端 JS 实现，
//! 用户写下的期望值都是按那套规则试出来的。换成 Rust 后若改用别的规则，
//! 已经跑通的用例会莫名其妙地开始失败——那是最难排查的一类回归。

use serde_json::Value;

/// 对应 JS 的 `String(v)`；结构体走 JSON 文本（JS 侧断言实现同样用 `JSON.stringify`）。
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number_text(n),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 数字的文本形式。`5.0` 要写成 `5`——JSON 里的 `5` 经 serde 可能落成浮点，
/// 而用户在断言里写的期望值是 `5`，多出的 `.0` 会让一条本该通过的断言失败。
pub fn number_text(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    match n.as_f64() {
        Some(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 => (f as i64).to_string(),
        Some(f) => f.to_string(),
        None => "null".into(),
    }
}

/// 对应 JS 的 `Number(v)`：数字原样、布尔 1/0、null 与空串 0、其余按十进制解析。
/// 解析不出返回 `None`（即 JS 的 `NaN`，参与任何比较都为 false）。
pub fn js_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        Value::String(s) => js_number_str(s),
        _ => None,
    }
}

/// 字符串转数字，同 `Number("...")`：两端空白忽略，空串是 0。
pub fn js_number_str(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return Some(0.0);
    }
    t.parse::<f64>().ok().filter(|f| f.is_finite())
}

/// 当前 Unix 毫秒。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Unix 毫秒 → ISO 8601（UTC，带毫秒与 `Z`）。
///
/// 刻意**只产出 UTC**，不引时区库：报告里的时间戳要能跨时区比对、跨机器归档，
/// 展示成本地时间是渲染层的事（报告页的 JS 用 `toLocaleString` 转）。
/// 为此自己算历法而不是拉 chrono——只需要这一个方向的转换，算法是确定的、可测的。
pub fn iso8601(ms: u64) -> String {
    let secs = ms / 1000;
    let sub = ms % 1000;
    let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{sub:03}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// 毫秒 → `YYYYMMDDHHmmss`，用作报告文件名的前缀（排序即时序）。
///
/// **传进来的毫秒被当作「墙上时间」直接格式化**，本函数不做时区转换：报告文件名是给人看的，
/// 显示成 UTC 会让「下午三点跑的那份」写着 07。调用方自行把本地偏移加进去
/// （前端用 `Date` 的本地取值，CLI 用系统时区偏移），core 仍不引时区库。
pub fn stamp14(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!("{y:04}{mo:02}{d:02}{:02}{:02}{:02}", tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// Unix 毫秒 → HTTP 日期（RFC 1123，形如 `Wed, 21 Oct 2026 07:28:00 GMT`）。
///
/// 用于拼 `Set-Cookie` 的 `Expires=`——管理界面手工加一条 cookie 时，
/// 交给 cookie 解析器去认这一串比自己去构造属性对象省事，也顺带走了它的合法性校验。
/// 同 `iso8601`：自己算历法，不引时区库（HTTP 日期恒是 GMT）。
pub fn http_date(ms: u64) -> String {
    const WDAY: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = ms / 1000;
    let days = secs / 86_400;
    let (y, mo, d) = civil_from_days(days as i64);
    // 1970-01-01 是星期四，故 +4 对齐到「0 = 星期日」
    let wd = ((days + 4) % 7) as usize;
    let tod = secs % 86_400;
    format!(
        "{}, {d:02} {} {y:04} {:02}:{:02}:{:02} GMT",
        WDAY[wd],
        MON[(mo - 1) as usize],
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Unix 毫秒 → **秒级** ISO 8601（`2026-08-12T03:04:05Z`）。
///
/// 与 `iso8601` 的差别只在不带毫秒：写进 `.apicase/cookies.yml` 的过期时间是给人看、
/// 给人改的，而 cookie 的 `Expires` 本来就只到秒——`.000` 是纯噪音。
pub fn iso8601_secs(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z", tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// ISO 8601 / RFC 3339 文本 → Unix 毫秒。**宽进**：这是手写文件里的字段。
///
/// 认得下列写法（缺失的部分按 0 补）：
/// `2026-08-12`、`2026-08-12T03:04`、`2026-08-12 03:04:05`、`2026-08-12T03:04:05.123Z`、
/// `2026-08-12T11:04:05+08:00`。**无时区标记时按 UTC**——写文件的人若要本地时间，
/// 带上偏移即可，猜时区只会让同一份文件在两台机器上意思不同。
///
/// 1970 之前与认不出的一律 `None`：cookie 的过期时间在 1970 前没有意义，
/// 而返回一个 0 会让它当场"已过期"、悄悄消失。
pub fn parse_iso8601(s: &str) -> Option<u64> {
    let t = s.trim();
    // 日期与时间的分隔可以是 T 或空格；没有时间部分就是当天 00:00:00 UTC
    let (date, rest) = match t.find(['T', 't', ' ']) {
        Some(i) => (&t[..i], t[i + 1..].trim()),
        None => (t, ""),
    };
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.trim().parse().ok()?;
    let mo: u32 = match dp.next() {
        Some(v) => v.trim().parse().ok()?,
        None => 1,
    };
    let d: u32 = match dp.next() {
        Some(v) => v.trim().parse().ok()?,
        None => 1,
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }

    // 尾部时区：Z（UTC）或 ±HH:MM / ±HHMM / ±HH。找符号要从时间部分里找，
    // 日期里的 `-` 不在这一段，故不会误判
    let (time, offset_min) = match rest.char_indices().find(|(i, c)| matches!(c, '+' | '-') && *i > 0) {
        Some((i, sign)) => {
            let tz = &rest[i + 1..];
            let mut p = tz.split(':');
            let h: i64 = p.next().and_then(|v| v.get(..2).unwrap_or(v).trim().parse().ok())?;
            let m: i64 = match p.next() {
                Some(v) => v.trim().parse().ok()?,
                // `+0800` 这种不带冒号的写法：分钟跟在小时后面
                None => tz.get(2..4).and_then(|v| v.parse().ok()).unwrap_or(0),
            };
            let mins = h * 60 + m;
            (&rest[..i], if sign == '-' { -mins } else { mins })
        }
        None => (rest.trim_end_matches(['Z', 'z']), 0),
    };

    let mut tp = time.trim().split(':');
    let hh: i64 = match tp.next().filter(|v| !v.is_empty()) {
        Some(v) => v.trim().parse().ok()?,
        None => 0,
    };
    let mm: i64 = match tp.next() {
        Some(v) => v.trim().parse().ok()?,
        None => 0,
    };
    // 秒可能带小数（`05.123`）——毫秒精度对 cookie 无意义，取整秒即可
    let ss: i64 = match tp.next() {
        Some(v) => v.split('.').next()?.trim().parse().ok()?,
        None => 0,
    };
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }

    let secs = days_from_civil(y, mo, d) * 86_400 + hh * 3600 + mm * 60 + ss - offset_min * 60;
    u64::try_from(secs).ok().map(|s| s.saturating_mul(1000))
}

/// 天序号（自 1970-01-01）→ 公历年月日。
/// Howard Hinnant 的 `civil_from_days`：把三月当年首，闰年规则因此退化成纯算术，
/// 没有查表、没有分支，闰年与世纪年自然正确。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]，以三月为 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 公历年月日 → 天序号（自 1970-01-01）。`civil_from_days` 的逆运算（同一篇算法）。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // 以三月为 0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_conversion_matches_javascript() {
        assert_eq!(js_string(&json!(200)), "200");
        assert_eq!(js_string(&json!(5.0)), "5", "整数值的浮点不带 .0");
        assert_eq!(js_string(&json!(1.5)), "1.5");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!(null)), "null");
        assert_eq!(js_string(&json!("x")), "x");
        assert_eq!(js_string(&json!({"a":1})), "{\"a\":1}");
        assert_eq!(js_string(&json!([1, 2])), "[1,2]");
    }

    /// 与 `new Date(ms).toISOString()` 逐字一致；闰年、世纪年、跨年边界都要对。
    #[test]
    fn iso8601_matches_javascript_toisostring() {
        let cases = [
            (0u64, "1970-01-01T00:00:00.000Z"),
            (1, "1970-01-01T00:00:00.001Z"),
            (1_785_369_600_000, "2026-07-30T00:00:00.000Z"),
            (1_785_456_896_789, "2026-07-31T00:14:56.789Z"),
            // 闰日（2024 是闰年）
            (1_709_164_800_000, "2024-02-29T00:00:00.000Z"),
            // 2000 是闰年、1900 不是——世纪年规则的经典陷阱
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            // 跨年的最后一毫秒
            (1_767_225_599_999, "2025-12-31T23:59:59.999Z"),
        ];
        for (ms, want) in cases {
            assert_eq!(iso8601(ms), want, "{ms}");
        }
    }

    /// 与 `new Date(ms).toUTCString()` 逐字一致（星期几算错的话，cookie 的过期时间会被服务端/解析器丢掉）
    #[test]
    fn http_date_matches_javascript_toutcstring() {
        let cases = [
            (0u64, "Thu, 01 Jan 1970 00:00:00 GMT"),
            (1_785_369_600_000, "Thu, 30 Jul 2026 00:00:00 GMT"),
            (1_785_456_896_789, "Fri, 31 Jul 2026 00:14:56 GMT"),
            (1_709_164_800_000, "Thu, 29 Feb 2024 00:00:00 GMT"),
            (951_782_400_000, "Tue, 29 Feb 2000 00:00:00 GMT"),
            (1_767_225_599_000, "Wed, 31 Dec 2025 23:59:59 GMT"),
        ];
        for (ms, want) in cases {
            assert_eq!(http_date(ms), want, "{ms}");
        }
    }

    /// 秒级 ISO：与 `iso8601` 同一套历法，只是去掉毫秒
    #[test]
    fn iso8601_secs_drops_the_milliseconds() {
        assert_eq!(iso8601_secs(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_secs(1_785_456_896_789), "2026-07-31T00:14:56Z");
        assert_eq!(iso8601_secs(1_709_164_800_000), "2024-02-29T00:00:00Z");
    }

    /// 写出去的能原样读回来——`.apicase/cookies.yml` 每保存一次就走一遍这条往返，
    /// 差一秒都会让过期时间每次落盘漂一点
    #[test]
    fn iso8601_round_trips_to_the_same_second() {
        for ms in [0u64, 1_785_369_600_000, 1_709_164_800_000, 1_767_225_599_000, 4_102_444_800_000] {
            assert_eq!(parse_iso8601(&iso8601_secs(ms)), Some(ms), "{ms}");
        }
        // 带毫秒的也认，毫秒部分丢掉（cookie 的过期只到秒）
        assert_eq!(parse_iso8601("2026-07-31T00:14:56.789Z"), Some(1_785_456_896_000));
    }

    /// 手写文件里的各种写法都要认——这是给人和 AI 改的字段，不该挑格式
    #[test]
    fn parse_iso8601_accepts_handwritten_forms() {
        let noon = 1_785_369_600_000; // 2026-07-30T00:00:00Z
        assert_eq!(parse_iso8601("2026-07-30"), Some(noon), "只有日期＝当天零点 UTC");
        assert_eq!(parse_iso8601("2026-07-30 00:00:00"), Some(noon), "空格分隔");
        assert_eq!(parse_iso8601("  2026-07-30T00:00Z  "), Some(noon), "省略秒 + 两端空白");
        // 带偏移：+08:00 的 08:00 就是 UTC 的零点
        assert_eq!(parse_iso8601("2026-07-30T08:00:00+08:00"), Some(noon));
        assert_eq!(parse_iso8601("2026-07-30T08:00:00+0800"), Some(noon), "不带冒号的偏移");
        assert_eq!(parse_iso8601("2026-07-29T16:00:00-08:00"), Some(noon), "负偏移");

        // 认不出的一律 None：宁可当作"没有过期时间"（会话 cookie），也不要算出个错的时刻
        for bad in ["", "下周三", "2026-13-01", "2026-07-32", "2026-07-30T25:00:00Z", "1969-12-31"] {
            assert_eq!(parse_iso8601(bad), None, "{bad}");
        }
    }

    /// 报告文件名的时间戳前缀：与 `iso8601` 取的是同一套历法，只是换个排版
    #[test]
    fn stamp14_is_the_iso_timestamp_without_separators() {
        for ms in [0u64, 1_785_369_600_000, 1_709_164_800_000, 1_767_225_599_999] {
            let want: String = iso8601(ms).chars().filter(char::is_ascii_digit).take(14).collect();
            assert_eq!(stamp14(ms), want, "{ms}");
        }
        assert_eq!(stamp14(1_785_369_600_000), "20260730000000");
    }

    #[test]
    fn number_conversion_matches_javascript() {
        assert_eq!(js_number(&json!("200")), Some(200.0));
        assert_eq!(js_number(&json!("  7 ")), Some(7.0));
        assert_eq!(js_number(&json!("")), Some(0.0), "Number('') 是 0");
        assert_eq!(js_number(&json!(null)), Some(0.0), "Number(null) 是 0");
        assert_eq!(js_number(&json!(true)), Some(1.0));
        assert_eq!(js_number(&json!("abc")), None, "NaN");
        assert_eq!(js_number(&json!({"a":1})), None);
    }
}
