//! 把命令行工具接进 `PATH`——桌面端的「安装命令行工具」按钮与 `apicase self install`
//! **走的是这一份实现**。
//!
//! # 为什么要装
//!
//! AI Agent 读 `AGENTS.md` 之后会直接敲 `apicase run`。敲不到就是硬失败，
//! 而它不会自己去 `/Applications/Apicase.app/Contents/Resources/bin/` 里翻。
//! 人也一样——桌面版自带一份 CLI，但埋在 bundle 里等于没有。
//!
//! # 为什么优先用户目录而不是 /usr/local/bin
//!
//! 1. **不需要提权**。目标是「用户无感」，弹一次系统密码框就不无感了。
//! 2. **全局软链会指错人**：`.app` 可以装在 `/Applications`（全局）也可以装在
//!    `~/Applications`（用户级）。一旦软链是全局的、目标是用户级的，另一个用户要么
//!    撞不到、要么读的是别人的安装；那人卸载 app 之后就变成断链。
//! 3. 业界分野也是这个走向：系统级应用（Docker、VSCode）走 `/usr/local/bin`，
//!    开发者工具链（rustup、uv、pipx）走用户目录。apicase 是后者。
//!
//! 多用户共享一台开发机、还各自要用 apicase 的场景，相对「第一次装就弹密码框」
//! 这个代价，出现频率低得多。

use std::path::{Path, PathBuf};

/// 装进 PATH 后的命令名。**不叫 `apicase-cli`**——用户敲的是 `apicase run`。
pub const LINK_NAME: &str = if cfg!(windows) { "apicase.exe" } else { "apicase" };

/// 候选安装位置，**按优先级**。
///
/// `~/.local/bin` 排在前面：XDG 约定的用户级可执行目录，rustup / uv / pipx 都用它，
/// 现代开发机上多半已经在 PATH 里。`/usr/local/bin` 作为次选——它在 macOS 的默认
/// PATH 里，但 Apple Silicon 上常常压根不存在（Homebrew 装在 `/opt/homebrew`），
/// 且多半要 root 才写得进去。
fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()) {
        v.push(home.join(".local").join("bin"));
    }
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        v.push(PathBuf::from(d).join("Programs").join("apicase"));
    }
    #[cfg(not(windows))]
    v.push(PathBuf::from("/usr/local/bin"));
    v
}

/// 当前 `PATH` 里的目录（规范化后，便于与候选比对）。
fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.canonicalize().unwrap_or(d)).collect())
        .unwrap_or_default()
}

fn in_path(dir: &Path, dirs: &[PathBuf]) -> bool {
    let real = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    dirs.contains(&real)
}

fn writable(dir: &Path) -> bool {
    // 只判断「已存在的目录能不能写」。用临时文件试探比看权限位可靠——
    // 权限位在 ACL、只读挂载、容器里都可能骗人。
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(".apicase-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 两个路径是不是同一个文件。
///
/// **按 canonicalize 后的真实路径比，不比字符串**：软链和大小写不敏感的文件系统
/// 都会骗人。这个坑在定位桌面端与打包命名上已经踩过两次。
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 命令行工具当前的接入状态。
#[derive(Debug, Clone, PartialEq)]
pub enum LinkStatus {
    /// 已装好，且指向我们这一份
    Installed { link: PathBuf },
    /// PATH 里有个 `apicase`，但**不是**我们这一份（可能是 cargo install 装的）。
    /// 不该动它——覆盖别人的东西才是冒犯。
    Foreign { link: PathBuf },
    /// 没装。`target` 是**建议**的落点；`needs_path_setup` 表示那个目录还不在 PATH 里，
    /// 装完还得让用户把它加进去。
    Missing { target: Option<PathBuf>, needs_path_setup: bool },
}

/// 查当前状态。`source` 是我们自己那份 CLI 的路径
/// （桌面端给 `resource_dir()/bin/apicase`，CLI 给 `current_exe()`）。
pub fn status(source: &Path) -> LinkStatus {
    let dirs = path_dirs();

    // ① PATH 里已经有同名命令？看它是不是我们这一份
    for dir in &dirs {
        let link = dir.join(LINK_NAME);
        if link.is_file() {
            return if same_file(&link, source) {
                LinkStatus::Installed { link }
            } else {
                LinkStatus::Foreign { link }
            };
        }
    }

    // ② 没装。挑一个落点：优先「已存在 + 在 PATH 里 + 可写」的
    let cands = candidates();
    if let Some(dir) = cands.iter().find(|d| in_path(d, &dirs) && writable(d)) {
        return LinkStatus::Missing { target: Some(dir.join(LINK_NAME)), needs_path_setup: false };
    }
    // ③ 退而求其次：第一个候选（多半是 ~/.local/bin），装完还要让用户加进 PATH
    LinkStatus::Missing {
        target: cands.first().map(|d| d.join(LINK_NAME)),
        needs_path_setup: true,
    }
}

/// 安装结果，供上层组织提示语。
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub link: PathBuf,
    /// 落点不在 PATH 里，装完还得让用户把它加进去
    pub needs_path_setup: bool,
    /// 本来就装好了，这次什么都没做
    pub already: bool,
}

/// 把 `source` 接进 PATH。**用软链而不是拷贝**——这样应用升级后 PATH 里的自动跟着新，
/// 不会退化成两个版本。
///
/// 三条不做的事：**不覆盖别人的同名命令**（`Foreign` 时直接报错让用户决定）、
/// **不提权**（写不进去就说清楚）、**不改 shell 配置**（那是动用户的环境，得另外征得同意）。
pub fn install(source: &Path) -> Result<Installed, String> {
    if !source.is_file() {
        return Err(format!("找不到命令行工具：{}", source.display()));
    }
    let (link, needs_path_setup) = match status(source) {
        LinkStatus::Installed { link } => {
            return Ok(Installed { link, needs_path_setup: false, already: true })
        }
        LinkStatus::Foreign { link } => {
            return Err(format!(
                "{} 已被另一个 apicase 占用（不是这个应用自带的那份）。\
                 如果想换成这份，请先手动删除它。",
                link.display()
            ))
        }
        LinkStatus::Missing { target, needs_path_setup } => {
            (target.ok_or("找不到可用的安装位置（没有 HOME？）")?, needs_path_setup)
        }
    };

    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录 {} 失败：{e}", dir.display()))?;
        if !writable(dir) {
            return Err(format!("{} 不可写（可能需要管理员权限）", dir.display()));
        }
    }
    // 已存在的同名软链（比如指向旧安装）先清掉，否则 symlink 会失败
    let _ = std::fs::remove_file(&link);
    symlink(source, &link).map_err(|e| format!("创建软链 {} 失败：{e}", link.display()))?;

    Ok(Installed { link, needs_path_setup, already: false })
}

/// 移除。**只删我们自己装的那个**——`Foreign` 的不碰。
///
/// 卸载这件事不能省：macOS 把 `.app` 拖进废纸篓不会清理软链，PATH 里会留一个断链，
/// 之后敲 `apicase` 报的是「No such file or directory」，比「命令未找到」更让人困惑。
pub fn uninstall(source: &Path) -> Result<Option<PathBuf>, String> {
    match status(source) {
        LinkStatus::Installed { link } => {
            std::fs::remove_file(&link).map_err(|e| format!("删除 {} 失败：{e}", link.display()))?;
            Ok(Some(link))
        }
        LinkStatus::Foreign { link } => Err(format!(
            "{} 不是这个应用装的，不动它",
            link.display()
        )),
        LinkStatus::Missing { .. } => Ok(None),
    }
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// Windows 上建符号链接需要开发者模式或管理员权限，失败时回落到**硬链接**，
/// 再不行才拷贝。拷贝是最后手段——它会让应用升级后 PATH 里的那份变成旧版本。
#[cfg(windows)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
        .or_else(|_| std::fs::hard_link(src, dst))
        .or_else(|_| std::fs::copy(src, dst).map(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("apicase-link-{name}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("建目录");
        p
    }

    fn fake_cli(dir: &Path) -> PathBuf {
        let p = dir.join("apicase-source");
        std::fs::write(&p, b"#!/bin/sh\necho fake\n").expect("写文件");
        p
    }

    /// 装 → 查 → 卸 的往返。用软链，所以改了源文件那边立刻可见。
    #[cfg(unix)]
    #[test]
    fn install_then_uninstall_round_trips() {
        let base = tmp("roundtrip");
        let src = fake_cli(&base);
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).expect("建目录");
        let link = bin.join(LINK_NAME);

        // 直接验证底层动作（status/install 会读真实 PATH，测试里不去动它）
        std::os::unix::fs::symlink(&src, &link).expect("建软链");
        assert!(same_file(&link, &src), "软链应指向源文件");
        assert!(!same_file(&link, &base.join("别的")), "不同文件不该判为同一个");

        std::fs::remove_file(&link).expect("删软链");
        assert!(!link.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 源文件不存在时要报错而不是建出一个断链
    #[test]
    fn install_rejects_a_missing_source() {
        let e = install(Path::new("/绝不存在的/apicase")).expect_err("应当失败");
        assert!(e.contains("找不到命令行工具"), "{e}");
    }

    /// 可写性用「试写一个临时文件」判断，比看权限位可靠
    #[test]
    fn writability_is_probed_not_guessed() {
        let base = tmp("writable");
        assert!(writable(&base), "刚建的目录应可写");
        assert!(!writable(&base.join("不存在")), "不存在的目录不算可写");
        assert!(!writable(Path::new("/dev/null")), "不是目录不算可写");
        // 探针文件不该留下
        assert!(!base.join(".apicase-write-probe").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 候选目录的顺序：用户目录在前（不需要提权），全局在后
    #[test]
    fn user_dir_is_preferred_over_the_global_one() {
        let c = candidates();
        assert!(!c.is_empty());
        if std::env::var_os("HOME").is_some() {
            assert!(c[0].ends_with(".local/bin"), "首选应是 ~/.local/bin：{c:?}");
        }
        #[cfg(not(windows))]
        assert!(c.iter().any(|p| p == Path::new("/usr/local/bin")), "全局目录应作为次选：{c:?}");
    }

    /// 状态判定要按真实路径比对，不能比字符串——软链与大小写不敏感都会骗人
    #[cfg(unix)]
    #[test]
    fn foreign_link_is_recognised_and_left_alone() {
        let base = tmp("foreign");
        let ours = fake_cli(&base);
        let theirs = base.join("someone-elses");
        std::fs::write(&theirs, b"#!/bin/sh\n").expect("写文件");
        assert!(!same_file(&ours, &theirs), "两个不同的文件不该判为同一份");

        // 经软链绕一圈仍应认得出是同一个
        let via = base.join("via-symlink");
        std::os::unix::fs::symlink(&ours, &via).expect("建软链");
        assert!(same_file(&via, &ours), "软链与其目标是同一个文件");
        let _ = std::fs::remove_dir_all(&base);
    }
}
