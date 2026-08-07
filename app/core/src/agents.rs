//! 生成 `AGENTS.md`——让**任何**有 shell 能力的 AI Agent 都能操作这个工作空间。
//!
//! # 为什么这条路比 MCP 更值得
//!
//! `AGENTS.md` 是 Linux Foundation 旗下 Agentic AI Foundation 的开放标准，
//! Codex / Cursor / Copilot / Gemini CLI / Windsurf / Zed / Aider / Devin 等
//! 三十多个 Agent 原生读它，Claude Code 也读。而它**零配置**——文件在目录里就生效、
//! 随 git 走到每个同事的机器上；MCP 则要每人在自己的客户端里配一次。
//!
//! # 指路，不复制
//!
//! 内容刻意做得短：**告诉 AI 去哪儿查，而不是把规范抄一份进来**。抄一份的后果有二：
//! 每次 AI 打开项目都吃掉几千 token；规范改了这里没改，就有了第二个真相。
//! 这与 MCP 那边把格式规范做成 `apicase_docs` 工具、而不是塞进 `instructions`
//! 是同一条原则。

use std::path::Path;

/// 标准文件名。**只生成这一个**——Claude Code 现在也读 `AGENTS.md`，
/// 再生成一份 `CLAUDE.md` 就是制造第二个真相。
pub const AGENTS_FILE: &str = "AGENTS.md";

/// 自己那一段的边界标记。有了它才能**幂等更新**：升级版本时只替换标记之间的内容，
/// 不动用户自己写的部分。
const BEGIN: &str = "<!-- apicase:begin -->";
const END: &str = "<!-- apicase:end -->";

/// 写入结果，供上层组织提示语。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// 新建了文件
    Created,
    /// 文件已有，把 apicase 那一段追加在末尾
    Appended,
    /// 已有 apicase 段落，内容有变，替换掉
    Updated,
    /// 已是最新，什么都没做
    Unchanged,
}

/// apicase 那一段的正文。
///
/// `cli` 是 AI 该敲的命令名——通常就是 `apicase`；当它还没进 PATH 时，
/// 传 `None` 会附上一段按**平台约定**（而非本机绝对路径）的兜底说明。
/// 绝不写本机路径：这份文件要随 git 走，写死 `/Users/某某/…` 到同事那儿就是错的。
pub fn section(cli_on_path: bool) -> String {
    let fallback = if cli_on_path {
        String::new()
    } else {
        "\n命令是 `apicase`。若 PATH 里没有，桌面版自带一份，在这些位置：\n\
         - macOS   `/Applications/Apicase.app/Contents/Resources/bin/apicase`\n\
         - Windows `%LOCALAPPDATA%\\Programs\\Apicase\\resources\\bin\\apicase.exe`\n\
         - Linux   `/usr/lib/apicase-desktop/bin/apicase`\n\n\
         找到后请提示用户运行 `apicase init` 或打开「设置 → AI → 安装命令行工具」，\
         装一次以后就不用再找。**不要自行下载安装。**\n"
            .to_string()
    };

    format!(
        "{BEGIN}\n\
## API 用例（apicase）\n\
\n\
本目录是 apicase 工作空间：一个 `.yml` = 一个用例，内部是 HTTP 请求的 DAG。\n\
用例是纯文本，**用你自己的文件读写工具直接编辑即可**。\n\
{fallback}\n\
| 命令 | 用途 |\n\
|---|---|\n\
| `apicase docs` | **写用例前先读**，YAML 格式规范（schema 靠猜必然写错） |\n\
| `apicase check` | 只解析不发请求，秒回。写完先过这一道 |\n\
| `apicase run --json` | 运行，返回统计与失败现场 |\n\
| `apicase ls` | 列出现有用例 |\n\
\n\
典型闭环：**查格式 → 写 .yml → check 自检 → run 验证 → 读失败现场再改**。\n\
\n\
登录态存在 `.apicase/cookies.yml`，是一份可直接编辑的 YAML（`apicase docs cookies` 看格式）——\n\
换 token、清会话都改这个文件，**没有对应的命令**。\n\
\n\
读结果时注意三种状态是三回事：\n\
`failed` 是请求发出去了但断言没过（**被测服务**的问题）、\n\
`error` 是请求本身失败（网络 / TLS / 超时，多半是**环境或用例自身**的问题）、\n\
`skipped` 是上游挂了没轮到它跑。\n\
{END}\n"
    )
}

/// 把 apicase 段落写进工作空间的 `AGENTS.md`。**幂等**。
///
/// 三种情形分开处理，核心是**绝不覆盖用户自己写的内容**——工作空间很可能就是某个大项目
/// 的根目录，那里的 `AGENTS.md` 讲的是整个项目的事。
pub fn write(root: &Path, cli_on_path: bool) -> Result<Written, String> {
    let path = root.join(AGENTS_FILE);
    let body = section(cli_on_path);

    let Ok(existing) = std::fs::read_to_string(&path) else {
        // 读不到就当没有（不存在、或权限不足——后者写的时候自然会报错）
        std::fs::write(&path, &body).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
        return Ok(Written::Created);
    };

    let (text, how) = match splice(&existing, &body) {
        Some(t) if t == existing => return Ok(Written::Unchanged),
        Some(t) => (t, Written::Updated),
        None => {
            // 没有标记：追加到末尾，中间留一个空行
            let sep = if existing.ends_with('\n') { "\n" } else { "\n\n" };
            (format!("{existing}{sep}{body}"), Written::Appended)
        }
    };

    std::fs::write(&path, text).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
    Ok(how)
}

/// 把标记之间的内容替换掉；没有标记返回 `None`。
///
/// 只认**成对且顺序正确**的标记。缺一半或反着写就当没有——那时追加一段新的，
/// 好过在残缺的标记上做切片、把用户的内容切掉一块。
fn splice(existing: &str, body: &str) -> Option<String> {
    let b = existing.find(BEGIN)?;
    let e = existing[b..].find(END).map(|i| b + i + END.len())?;
    let mut out = String::with_capacity(existing.len() + body.len());
    out.push_str(&existing[..b]);
    out.push_str(body.trim_end_matches('\n'));
    out.push_str(&existing[e..]);
    Some(out)
}

/// 工作空间里 apicase 那一段的状态（界面上显示用）。
#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum State {
    /// 文件不存在，或存在但没有 apicase 段落
    Absent,
    /// 段落在，但内容不是当前版本——**升级后的常态**。
    /// 与「已配置」分开报，否则用户看着绿灯，AI 手上却是旧说明。
    Stale,
    /// 段落在且已是最新
    Ready,
}

/// 段落状态。`cli_on_path` 与 `section` 同义——它决定正文里有没有那段兜底路径，
/// 传错会让「装完 CLI」凭空变成一次「不一致」。
pub fn state(root: &Path, cli_on_path: bool) -> State {
    let Ok(existing) = std::fs::read_to_string(root.join(AGENTS_FILE)) else {
        return State::Absent;
    };
    match splice(&existing, &section(cli_on_path)) {
        Some(t) if t == existing => State::Ready,
        Some(_) => State::Stale,
        None => State::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("apicase-agents-{name}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("建目录");
        p
    }

    #[test]
    fn creates_then_stays_idempotent() {
        let root = tmp("create");
        assert_eq!(state(&root, true), State::Absent);

        assert_eq!(write(&root, true).expect("写入"), Written::Created);
        assert_eq!(state(&root, true), State::Ready);
        assert_eq!(write(&root, true).expect("再写"), Written::Unchanged, "第二次不该有改动");

        let text = std::fs::read_to_string(root.join(AGENTS_FILE)).expect("读");
        for must in
            ["apicase docs", "apicase check", "apicase run --json", "failed", "skipped", ".apicase/cookies.yml"]
        {
            assert!(text.contains(must), "应含 `{must}`");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 工作空间可能就是某个大项目的根，那里已有一份讲整个项目的 AGENTS.md。
    /// **绝不能覆盖它**。
    #[test]
    fn appends_to_an_existing_file_without_touching_it() {
        let root = tmp("append");
        let theirs = "# 我的项目\n\n构建：`make all`\n";
        std::fs::write(root.join(AGENTS_FILE), theirs).expect("写");

        assert_eq!(write(&root, true).expect("写入"), Written::Appended);
        let text = std::fs::read_to_string(root.join(AGENTS_FILE)).expect("读");
        assert!(text.starts_with(theirs), "用户原有内容要一字不动地留在前面：\n{text}");
        assert!(text.contains(BEGIN) && text.contains(END));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 升级时只替换自己那一段，用户写在前后的内容都保住
    #[test]
    fn updates_only_its_own_section() {
        let root = tmp("update");
        let file = root.join(AGENTS_FILE);
        std::fs::write(&file, format!("前言\n\n{BEGIN}\n旧内容\n{END}\n\n后记\n")).expect("写");

        assert_eq!(write(&root, true).expect("写入"), Written::Updated);
        let text = std::fs::read_to_string(&file).expect("读");
        assert!(text.starts_with("前言\n"), "前面的内容要留住：\n{text}");
        assert!(text.trim_end().ends_with("后记"), "后面的内容也要留住：\n{text}");
        assert!(!text.contains("旧内容"), "自己那一段应被替换");
        assert_eq!(text.matches(BEGIN).count(), 1, "标记不该重复");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 内容是旧版本的要报「不一致」而不是「已配置」——后者会让用户以为 AI 手上是新说明
    #[test]
    fn an_outdated_section_reads_as_stale() {
        let root = tmp("stale");
        std::fs::write(root.join(AGENTS_FILE), format!("{BEGIN}\n旧版本的说明\n{END}\n")).expect("写");
        assert_eq!(state(&root, true), State::Stale);

        assert_eq!(write(&root, true).expect("写入"), Written::Updated);
        assert_eq!(state(&root, true), State::Ready, "更新过就该是最新");
        // CLI 在不在 PATH 会改变正文（兜底路径那一段），状态跟着变
        assert_eq!(state(&root, false), State::Stale);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 标记残缺时当作没有——在半个标记上做切片会把用户的内容切掉一块
    #[test]
    fn a_broken_marker_is_treated_as_absent() {
        let root = tmp("broken");
        std::fs::write(root.join(AGENTS_FILE), format!("正文\n{BEGIN}\n没有结束标记\n")).expect("写");
        assert_eq!(write(&root, true).expect("写入"), Written::Appended, "应追加而不是切片");
        let text = std::fs::read_to_string(root.join(AGENTS_FILE)).expect("读");
        assert!(text.contains("没有结束标记"), "用户内容不该被切掉：\n{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// CLI 不在 PATH 时附兜底路径，且**只用平台约定路径**——
    /// 写本机绝对路径的话，这份文件随 git 到同事那儿就是错的
    #[test]
    fn fallback_paths_are_conventional_not_local() {
        let with = section(false);
        assert!(with.contains("/Applications/Apicase.app"), "应给 macOS 的约定位置");
        assert!(with.contains("不要自行下载安装"), "要挡住 AI 自己去下载");
        assert!(!with.contains("/Users/"), "绝不能出现本机路径：\n{with}");

        let without = section(true);
        assert!(!without.contains("/Applications/"), "已在 PATH 时不该有兜底段落");
        assert!(without.len() < with.len());
    }
}
