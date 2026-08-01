//! 把工作空间里的 case 文件迁移到最新格式。
//!
//! 做三件事，都是**语义保持**的：
//!
//! 1. **变量语法** `{{var}}` → `${{var}}`。`{` 是 YAML 的流式映射起始指示符，
//!    以 `{{` 开头的值必须整行加引号（`url: '{{baseUrl}}/get'`）；`$` 不在指示符表里，
//!    换掉之后 `url: ${{baseUrl}}/get` 可以裸写。执行内核也已只认带 `$` 的写法。
//! 2. **`apicase: '0.1'`** → `apicase: v0.1`。`0.1` 是数字形态，作为字符串落盘就得加引号，
//!    真到 `0.10` 时裸写还会被读成 `0.1`。`v0.1` 本就不是数字。
//! 3. **断言目标**统一到 `res` 命名空间：`status` → `res.status`、
//!    `header.X` → `res.headers.X`、`$.a.b` → `res.body.a.b`。执行内核已只认
//!    `res.*`，旧写法一律取不到值——这条迁移是硬切换的配套，不跑就会静默失效。
//! 4. **画布坐标**从顶层 `ui.nodes.<stepId>` 挪进各 step 的 `ui:`。解析器已不认
//!    顶层写法，所以这条得在**文本层**先把坐标捞出来（`parse_case` 之后就没了），
//!    再按 id 灌回对应 step。捞不到 id 的坐标（step 已删）随之丢弃。
//!
//! 随后经 `parse_case` → `dump_case` 走一遍，顺带把格式规范化（引号规则、序列缩进、
//! 流式映射展开、默认值裁剪）。**语义等价由程序自己校验**：迁移前后的模型必须逐字段相同
//! （变量语法与版本号这两处是刻意改动，校验时归一化后比对）。
//!
//! ```sh
//! cargo run -p apicase-core --example migrate -- <工作空间>          # 预演，不写盘
//! cargo run -p apicase-core --example migrate -- <工作空间> --write   # 真正写入
//! ```

use apicase_core::yaml;
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().ok_or("用法：migrate <工作空间> [--write]")?);
    let write = args.any(|a| a == "--write");

    let mut files = Vec::new();
    discover(&root, &mut files);
    files.sort();
    println!("{} 个 yml 文件{}\n", files.len(), if write { "（写入模式）" } else { "（预演，不写盘）" });

    let mut changed = 0;
    let mut skipped = 0;
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
        let src = std::fs::read_to_string(f)?;

        // application.yml 不是 case，只做变量语法替换（它的 environment 值里也可能有引用）
        let is_case = f.file_name().is_some_and(|n| n != "application.yml");
        if !is_case {
            let next = upgrade_vars(&src);
            report(&rel, &src, &next, write, f, &mut changed, &mut skipped)?;
            continue;
        }

        // 不是有效 case（比如手写坏了）就跳过，别拿 dump 去覆盖人家的原文
        let analyzed = yaml::analyze_case(&src);
        if !analyzed.valid {
            println!("  – {rel}  跳过：{}", analyzed.error.unwrap_or_default());
            skipped += 1;
            continue;
        }

        let before = analyzed.case.expect("valid 时必有 case");
        let mut migrated = yaml::parse_case(&upgrade_version(&upgrade_vars(&src)))?;
        upgrade_targets(&mut migrated); // 断言目标在模型层改：文本替换分不清 target 与 outputs 的路径
        adopt_legacy_ui(&mut migrated, &src); // 坐标要先从原文里捞——解析器已经不认顶层 ui: 了
        let next = yaml::dump_case(&migrated);

        // 语义校验：迁移前后必须等价（把两处刻意改动归一化后比对）
        let after = yaml::parse_case(&next)?;
        if normalize(&before) != normalize(&after) {
            println!("  ✗ {rel}  语义不等价，已跳过（请手工检查）");
            skipped += 1;
            continue;
        }
        report(&rel, &src, &next, write, f, &mut changed, &mut skipped)?;
    }

    println!("\n{} 个文件需要更新，{} 个跳过", changed, skipped);
    if !write && changed > 0 {
        println!("加 --write 真正写入");
    }
    Ok(())
}

fn report(
    rel: &str,
    src: &str,
    next: &str,
    write: bool,
    path: &Path,
    changed: &mut usize,
    _skipped: &mut usize,
) -> std::io::Result<()> {
    if next == src {
        println!("  · {rel}  已是最新");
        return Ok(());
    }
    *changed += 1;
    let quotes_before = src.matches('\'').count() / 2 + src.matches('"').count() / 2;
    let quotes_after = next.matches('\'').count() / 2 + next.matches('"').count() / 2;
    println!("  ✓ {rel}  引号 {quotes_before} → {quotes_after}");
    if write {
        std::fs::write(path, next)?;
    }
    Ok(())
}

/// `{{var}}` → `${{var}}`，但**跳过已经带 `$` 的**（幂等，重复跑不会变成 `$${{`）。
fn upgrade_vars(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' && i + 1 < b.len() && b[i + 1] == b'{' {
            // 前面已经有 `$` 了就原样搬过去
            let already = out.ends_with('$');
            if !already {
                out.push('$');
            }
            out.push_str("{{");
            i += 2;
            continue;
        }
        let l = utf8_len(b[i]);
        out.push_str(&s[i..i + l]);
        i += l;
    }
    out
}

/// `apicase: 0.1` / `'0.1'` / `"0.1"` → `apicase: v0.1`（只认已知的老值，不乱改自定义版本）。
fn upgrade_version(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("apicase:") {
            let v = rest.trim().trim_matches(['\'', '"']);
            if v == "0.1" {
                let indent = &line[..line.len() - t.len()];
                out.push_str(&format!("{indent}apicase: {}", apicase_core::CASE_VERSION));
                continue;
            }
        }
        out.push_str(line);
    }
    if s.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 断言目标迁移到 `res` 命名空间。已是 `res.` 开头的原样返回（幂等，可重复跑）；
/// 认不出的写法也原样留着——宁可让它显式失效等人来看，也不猜一个可能更错的目标。
///
/// `$.headers['Content-Type']` 会变成 `res.body.headers['Content-Type']` 而不是
/// `res.headers[...]`：`$` 一向指响应体根，httpbin 这类回显接口的 body 里正好也有个
/// `headers` 字段，映射成响应头就把语义改了。
fn upgrade_target(t: &str) -> String {
    let s = t.trim();
    if s == "res" || s.starts_with("res.") {
        return s.to_string();
    }
    if s == "status" {
        return "res.status".into();
    }
    if let Some(name) = s.strip_prefix("header.") {
        return format!("res.headers.{name}");
    }
    if let Some(rest) = s.strip_prefix('$') {
        return format!("res.body{rest}"); // `$` → res.body、`$.a` → res.body.a、`$['a']` → res.body['a']
    }
    s.to_string()
}

fn upgrade_targets(c: &mut apicase_core::Case) {
    for step in &mut c.requests {
        for a in &mut step.assertions {
            a.target = upgrade_target(&a.target);
        }
    }
}

/// 把旧格式顶层 `ui.nodes.<stepId>` 的坐标按 id 灌进各 step。
///
/// 只在 step 自己还没有坐标时灌（新写法优先），捞不到 id 的坐标随之丢弃——
/// 那是已被删掉的 step 留下的孤儿，正是这次改格式要根除的东西。
fn adopt_legacy_ui(c: &mut apicase_core::Case, src: &str) {
    let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(src) else { return };
    let Some(nodes) = v.get("ui").and_then(|u| u.get("nodes")).and_then(|n| n.as_mapping()) else { return };
    for step in &mut c.requests {
        if step.ui.is_some() {
            continue;
        }
        let Some(p) = nodes.get(serde_yaml::Value::String(step.id.clone())) else { continue };
        if let (Some(x), Some(y)) = (num_of(p.get("x")), num_of(p.get("y"))) {
            step.ui = Some(apicase_core::StepUi { x, y });
        }
    }
}

fn num_of(v: Option<&serde_yaml::Value>) -> Option<f64> {
    v?.as_f64()
}

/// 把「刻意改动的那几处」归一化，好让语义校验只关心真正的内容。
///
/// 对**整个 case** 做而不是只对报文：`docs` 里也常有 `{{var}}` 形式的说明文字
/// （"token 取自环境变量 {{token}}"），迁移同样会把它们换成新语法——那是对的
/// （docs 是给人看的示例，该展示当前语法），但校验时得一并归一化，
/// 否则这类文件会被误判成"语义变了"。
fn normalize(c: &apicase_core::Case) -> apicase_core::Case {
    let json = serde_json::to_string(c).unwrap_or_default().replace("${{", "{{");
    let mut out: apicase_core::Case = serde_json::from_str(&json).unwrap_or_else(|_| c.clone());
    out.version = apicase_core::CASE_VERSION.into();
    upgrade_targets(&mut out); // 迁移前的旧目标同样归一化，否则每个带断言的文件都会被判"语义变了"
    // 坐标不参与语义比对：它是视图态，而且迁移前后本来就分别是"顶层有 / step 上有"
    out.requests.iter_mut().for_each(|s| s.ui = None);
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn discover(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "dist") {
            continue;
        }
        let p = ent.path();
        if p.is_dir() {
            discover(&p, out);
        } else if matches!(p.extension().and_then(|e| e.to_str()), Some("yml") | Some("yaml")) {
            out.push(p);
        }
    }
}
