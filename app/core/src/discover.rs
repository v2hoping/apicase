//! 用例发现：从一个目录或文件解析出「这一轮要跑哪些 case 文件」。
//!
//! 这套规则此前有三份——`examples/run_workspace.rs` 的 `discover`、前端的 `discoverCases`、
//! 命令层 `list_dir` 的过滤表。CLI 会让它变成第四份，故收在这里。
//!
//! **显式指定的文件不做任何过滤**（同 `grep -r`：点名给的一定处理）。
//! `apicase run application.yml` 因此会得到一条「缺少 steps 列表」的跳过记录，
//! 而不是被静默丢掉——静默跳过会让人误以为跑过了。过滤只在**遍历目录**时生效。

use std::path::{Path, PathBuf};

/// 遍历时恒不下探的目录名。与文件树的「显示隐藏文件」无关：
/// 这几个目录里即便真躺着 `.yml` 也不是用例（依赖、构建产物）。
pub const SKIP_DIRS: [&str; 3] = ["node_modules", "target", "dist"];

/// 工作空间配置文件名。它是配置不是用例，遍历时排除。
pub const CONFIG_FILE: &str = "application.yml";

/// 扩展名是 `.yml` / `.yaml`（大小写不敏感）。
pub fn is_yaml_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("yml") || e.eq_ignore_ascii_case("yaml"))
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 遍历目录时是否收下这个文件：YAML、非隐藏、不是工作空间配置。
fn takes_in_walk(p: &Path) -> bool {
    let name = file_name(p);
    is_yaml_file(p) && !name.starts_with('.') && !name.eq_ignore_ascii_case(CONFIG_FILE)
}

/// 遍历时是否进入这个子目录：非隐藏、不在 `SKIP_DIRS` 里。
fn enters(name: &str) -> bool {
    !name.starts_with('.') && !SKIP_DIRS.contains(&name)
}

/// 发现一个目标下的用例文件，**按路径排序**（顺序可预期，两次运行的报告可比对）。
///
/// - `target` 是文件：原样返回它（不过滤，见模块文档）。
/// - `target` 是目录：遍历收集；`recursive == false` 时只看这一层。
/// - `target` 不存在：返回空表。调用方据此报错——这里返回 `Result` 没有意义，
///   目标存不存在调用方自己 `exists()` 一下就知道，而遍历途中读不动的子目录本就该跳过。
pub fn find_cases(target: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if target.is_file() {
        out.push(target.to_path_buf());
        return out;
    }
    if target.is_dir() {
        walk(target, recursive, &mut out);
        out.sort();
    }
    out
}

/// 多个目标合并发现：逐个展开后**去重**（`apicase run api api/login.yml` 不该把
/// 同一个用例跑两遍），保持首次出现的顺序。
pub fn find_all(targets: &[PathBuf], recursive: bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for t in targets {
        for p in find_cases(t, recursive) {
            // 目标数量是个位数、用例数量是百级，线性查找比建索引更简单也够快
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    // 单个目录读不动（权限 / 竞态删除）不该中断整轮发现
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        // 用 DirEntry 的 file_type 而不是 Path::is_dir：后者跟随符号链接，
        // 一个指回上级的链接会让遍历原地打转
        let Ok(ft) = ent.file_type() else { continue };
        if ft.is_dir() {
            if recursive && enters(&file_name(&p)) {
                walk(&p, recursive, out);
            }
        } else if takes_in_walk(&p) {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一棵目录树，返回根路径。测试用完自行清理。
    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let base = std::env::temp_dir().join(format!("apicase-discover-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        for f in files {
            let p = base.join(f);
            std::fs::create_dir_all(p.parent().expect("有父目录")).expect("建目录");
            std::fs::write(&p, b"apicase: v0.1\nsteps: []\n").expect("写文件");
        }
        base
    }

    #[test]
    fn walk_filters_and_sorts() {
        let root = tree(
            "walk",
            &[
                "b.yml",
                "a.yaml",
                "application.yml",     // 配置不是用例
                "api/login.yml",       // 递归收下
                ".hidden/secret.yml",  // 隐藏目录
                ".env.yml",            // 隐藏文件
                "node_modules/dep.yml",
                "target/out.yml",
                "dist/bundle.yml",
                "readme.md",           // 非 YAML
            ],
        );

        let got: Vec<String> = find_cases(&root, true)
            .iter()
            .map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(got, vec!["a.yaml", "api/login.yml", "b.yml"], "按路径排序、过滤掉不该跑的");

        let shallow: Vec<String> = find_cases(&root, false)
            .iter()
            .map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(shallow, vec!["a.yaml", "b.yml"], "非递归只看这一层");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 显式点名的文件不过滤——被静默丢掉的话，用户会以为它跑过了
    #[test]
    fn explicit_file_is_never_filtered() {
        let root = tree("explicit", &["application.yml", ".hidden/x.yml", "notes.md"]);
        for rel in ["application.yml", ".hidden/x.yml", "notes.md"] {
            assert_eq!(find_cases(&root.join(rel), true), vec![root.join(rel)], "{rel}");
        }
        assert!(find_cases(&root.join("不存在.yml"), true).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 目标重叠时不该把同一个用例跑两遍
    #[test]
    fn multiple_targets_are_deduped_in_order() {
        let root = tree("dedup", &["api/login.yml", "api/order.yml", "smoke.yml"]);
        let got = find_all(&[root.join("smoke.yml"), root.join("api"), root.join("api/login.yml")], true);
        assert_eq!(
            got,
            vec![root.join("smoke.yml"), root.join("api/login.yml"), root.join("api/order.yml")],
            "保持首次出现的顺序，重复的不再追加"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
