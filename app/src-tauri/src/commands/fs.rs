//! 文件与目录命令：文件树、case 读写、增删改、搜索。
//!
//! 这些是**桌面壳专属**能力（core 里没有对应物）——CLI 直接用标准库读盘即可，
//! 用不着经过一层命令。放在这里的都是「界面需要、core 不该关心」的部分：
//! 隐藏项过滤、噪声目录跳过、二进制嗅探，全是为了文件树好看好用。

use serde::Serialize;
use std::path::Path;

/// 目录项（文件树节点）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// `.` 开头的隐藏项；前端据此淡色渲染（仅在 show_hidden 打开时才会出现）
    pub hidden: bool,
}

/// 即便打开「显示隐藏文件」也永远不列的目录。
/// `.git` 展开是几千个对象文件，把文件树彻底淹掉——「显示隐藏文件」不等于「显示一切」。
const ALWAYS_SKIP_DIRS: [&str; 4] = [".git", "node_modules", "target", "dist"];

/// Tauri 命令：列出某目录下的直接子项（文件树懒加载用）。
/// 默认跳过隐藏项（`.` 开头，如 .DS_Store）；`show_hidden` 打开后一并列出，
/// 但 `.git` 等噪声目录恒不列（见 ALWAYS_SKIP_DIRS）。目录在前，组内按名称（不区分大小写）排序。
#[tauri::command]
pub fn list_dir(path: String, show_hidden: Option<bool>) -> Result<Vec<DirEntry>, String> {
    let show_hidden = show_hidden.unwrap_or(false);
    let dir = std::path::Path::new(&path);
    if !dir.is_dir() {
        return Err(format!("不是目录: {path}"));
    }
    let mut entries: Vec<DirEntry> = Vec::new();
    for ent in std::fs::read_dir(dir).map_err(|e| format!("读取目录失败: {e}"))? {
        let ent = ent.map_err(|e| format!("读取目录项失败: {e}"))?;
        let name = ent.file_name().to_string_lossy().to_string();
        let hidden = name.starts_with('.');
        if hidden && !show_hidden {
            continue;
        }
        let p = ent.path();
        let is_dir = p.is_dir();
        if is_dir && ALWAYS_SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        entries.push(DirEntry {
            name,
            path: p.to_string_lossy().to_string(),
            is_dir,
            hidden,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Tauri 命令：读取文本文件内容（case 解析用）。
#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("读取文件失败: {e}"))
}

/// 智能读取的结果：要么是文本，要么判定为二进制/不支持编码。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    /// 二进制或不受支持的文本编码 —— 前端应显示占位提示而非编辑器
    pub binary: bool,
    /// 文本内容（binary=true 时为 None）
    pub text: Option<String>,
}

/// Tauri 命令：读文件并判定文本/二进制（仿 VSCode）。
/// 规则：前 64KB 含 NUL 字节 → 二进制（提前返回，不读大文件）；否则整体验 UTF-8，失败即"不支持的编码"。
#[tauri::command]
pub fn read_file_smart(path: String) -> Result<FileContent, String> {
    use std::io::Read;
    const SNIFF: usize = 64 * 1024;
    let mut file = std::fs::File::open(&path).map_err(|e| format!("读取文件失败: {e}"))?;
    let mut buf = vec![0u8; SNIFF];
    let n = file.read(&mut buf).map_err(|e| format!("读取文件失败: {e}"))?;
    buf.truncate(n);
    // NUL 字节是二进制的强特征（UTF-16 文本的 ASCII 区也含 NUL，一并归为不支持编码）
    if buf.contains(&0) {
        return Ok(FileContent { binary: true, text: None });
    }
    // 无 NUL：读完剩余部分再整体验 UTF-8
    file.read_to_end(&mut buf).map_err(|e| format!("读取文件失败: {e}"))?;
    match String::from_utf8(buf) {
        Ok(text) => Ok(FileContent { binary: false, text: Some(text) }),
        Err(_) => Ok(FileContent { binary: true, text: None }),
    }
}

/// Tauri 命令：写入文本文件（保存 case；存在即覆盖）。
#[tauri::command]
pub fn write_text_file(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, content).map_err(|e| format!("写入文件失败: {e}"))
}

/// Tauri 命令：新建文件（拒绝覆盖已存在，用于新建 case）。
#[tauri::command]
pub fn create_file(path: String, content: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if p.exists() {
        return Err(format!("已存在: {path}"));
    }
    std::fs::write(p, content).map_err(|e| format!("新建文件失败: {e}"))
}

/// Tauri 命令：新建目录（用于新建 folder；拒绝覆盖已存在）。
#[tauri::command]
pub fn create_dir(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if p.exists() {
        return Err(format!("已存在: {path}"));
    }
    std::fs::create_dir(p).map_err(|e| format!("新建目录失败: {e}"))
}

/// Tauri 命令：重命名 / 移动路径（文件或目录）。
#[tauri::command]
pub fn rename_path(from: String, to: String) -> Result<(), String> {
    if !std::path::Path::new(&from).exists() {
        return Err(format!("源路径不存在: {from}"));
    }
    if std::path::Path::new(&to).exists() {
        return Err(format!("目标已存在: {to}"));
    }
    std::fs::rename(&from, &to).map_err(|e| format!("重命名失败: {e}"))
}

/// 递归复制目录内容（隐藏项一并复制——只有 `list_dir` 出于展示需要跳过 `.` 开头）。
fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for ent in std::fs::read_dir(from)? {
        let ent = ent?;
        let src = ent.path();
        let dst = to.join(ent.file_name());
        if ent.file_type()?.is_dir() {
            copy_dir_all(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Tauri 命令：复制路径（文件或目录，用于「克隆」与「复制 / 粘贴」）。
/// 目标唯一名由前端算好；这里仍再校验一次目标已存在与「复制进自己的子目录」（否则会无限递归）。
#[tauri::command]
pub fn copy_path(from: String, to: String) -> Result<(), String> {
    let src = Path::new(&from);
    let dst = Path::new(&to);
    if !src.exists() {
        return Err(format!("源路径不存在: {from}"));
    }
    if dst.exists() {
        return Err(format!("目标已存在: {to}"));
    }
    if src.is_dir() && dst.starts_with(src) {
        return Err("不能把目录复制到它自己或它的子目录中".to_string());
    }
    if src.is_dir() {
        copy_dir_all(src, dst).map_err(|e| format!("复制目录失败: {e}"))
    } else {
        std::fs::copy(src, dst)
            .map(|_| ())
            .map_err(|e| format!("复制文件失败: {e}"))
    }
}

/// Tauri 命令：删除路径（文件用 remove_file，目录递归删除）。
#[tauri::command]
pub fn delete_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if p.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| format!("删除目录失败: {e}"))
    } else if p.exists() {
        std::fs::remove_file(p).map_err(|e| format!("删除文件失败: {e}"))
    } else {
        Err(format!("路径不存在: {path}"))
    }
}

/// Tauri 命令：在工作空间内递归搜索名称匹配（不区分大小写）的文件/目录（搜索栏用）。
/// 跳过隐藏项与常见大目录（node_modules/target/dist）；结果数上限 200。
#[tauri::command]
pub fn search_workspace(root: String, query: String) -> Result<Vec<DirEntry>, String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let root_path = std::path::Path::new(&root);
    if !root_path.is_dir() {
        return Err(format!("不是目录: {root}"));
    }
    const LIMIT: usize = 200;
    let mut out: Vec<DirEntry> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let p = ent.path();
            let is_dir = p.is_dir();
            if is_dir && name != "node_modules" && name != "target" && name != "dist" {
                stack.push(p.clone());
            }
            if name.to_lowercase().contains(&q) {
                out.push(DirEntry {
                    name,
                    path: p.to_string_lossy().to_string(),
                    is_dir,
                    hidden: false, // 搜索恒跳过隐藏项（上面已 continue），故此处必为 false
                });
                if out.len() >= LIMIT {
                    return Ok(out);
                }
            }
        }
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// Tauri 命令：判断路径是否存在（外部删除检测用）。
#[tauri::command]
pub fn path_exists(path: String) -> bool {
    Path::new(&path).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_path_files_and_dirs() {
        let base = std::env::temp_dir().join("apicase-copy-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src/sub")).expect("建测试目录");
        std::fs::write(base.join("src/a.yml"), b"a").expect("写文件");
        std::fs::write(base.join("src/sub/b.yml"), b"b").expect("写文件");
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();

        // 文件
        copy_path(s(&base.join("src/a.yml")), s(&base.join("a-copy.yml"))).expect("复制文件应成功");
        assert_eq!(std::fs::read(base.join("a-copy.yml")).unwrap(), b"a");

        // 目录（含子目录与其中的文件）
        copy_path(s(&base.join("src")), s(&base.join("dst"))).expect("复制目录应成功");
        assert_eq!(std::fs::read(base.join("dst/sub/b.yml")).unwrap(), b"b");

        // 目标已存在 → 拒绝（不覆盖）
        assert!(copy_path(s(&base.join("src/a.yml")), s(&base.join("a-copy.yml"))).is_err());
        // 目录复制进自己的子目录 → 拒绝（否则无限递归）
        let err = copy_path(s(&base.join("src")), s(&base.join("src/sub/self"))).expect_err("应拒绝");
        assert!(err.contains("自己"), "错误信息应说明原因: {err}");
        // 源不存在 → 拒绝
        assert!(copy_path(s(&base.join("nope")), s(&base.join("x"))).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// list_dir：默认跳过隐藏项；show_hidden 打开后列出并标记，但 .git 等噪声目录恒不列
    #[test]
    fn list_dir_hidden_toggle() {
        let base = std::env::temp_dir().join("apicase-listdir-hidden-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".apicase/reports")).expect("建目录");
        std::fs::create_dir_all(base.join(".git")).expect("建目录");
        std::fs::create_dir_all(base.join("node_modules")).expect("建目录");
        std::fs::create_dir_all(base.join("01-登录")).expect("建目录");
        std::fs::write(base.join("application.yml"), b"x").expect("写文件");
        std::fs::write(base.join(".gitignore"), b"x").expect("写文件");

        let root = base.to_string_lossy().into_owned();

        // 默认：隐藏项一个不列
        let plain = list_dir(root.clone(), None).expect("列目录应成功");
        let names: Vec<&str> = plain.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["01-登录", "application.yml"]);
        assert!(plain.iter().all(|e| !e.hidden), "默认列出的都不是隐藏项");

        // 打开后：隐藏项出现并被标记，.git / node_modules 仍不列
        let shown = list_dir(root.clone(), Some(true)).expect("列目录应成功");
        let names: Vec<&str> = shown.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec![".apicase", "01-登录", ".gitignore", "application.yml"]);
        assert!(!names.contains(&".git"), ".git 恒不列（展开是几千个对象文件）");
        assert!(!names.contains(&"node_modules"), "node_modules 恒不列");
        let ap = shown.iter().find(|e| e.name == ".apicase").expect("应有 .apicase");
        assert!(ap.hidden && ap.is_dir, ".apicase 被标记为隐藏目录");
        let yml = shown.iter().find(|e| e.name == "application.yml").expect("应有 application.yml");
        assert!(!yml.hidden, "普通文件不标记为隐藏");

        let _ = std::fs::remove_dir_all(&base);
    }
}
