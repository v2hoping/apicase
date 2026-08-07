//! 设置页「AI」分区的命令：把命令行工具接进 PATH、往工作空间生成 `AGENTS.md`。
//!
//! 同这个 crate 的其余部分一样，**没有一行业务逻辑**——判断哪个目录能装、
//! `AGENTS.md` 该追加还是替换，全在 `apicase-core`（`cli_link` / `agents`），
//! 与 `apicase self install` / `apicase init` 走的是同一份。
//!
//! 这一层只回答一个 core 不知道的问题：**我们自带的那份 CLI 在哪**。
//! 桌面端知道自己在哪，于是也就知道 bundle 里那份在哪——这是它比 CLI 更有资格
//! 做这件事的地方（CLI 只能猜桌面端装哪了，桌面端不用猜）。

use apicase_core::{agents, cli_link};
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// 自带的那份 CLI。
///
/// 打包后在 `Contents/Resources/bin/apicase`；`npm run tauri dev` 时 Tauri 同样会把
/// resources 复制到可执行文件旁边，所以这条路径开发期一样成立。
/// 只有绕过 tauri CLI 直接 `cargo run` 时才落到第二条兜底上。
fn bundled_cli(app: &AppHandle) -> Option<PathBuf> {
    let name = cli_link::LINK_NAME;
    let from_resources = app.path().resource_dir().ok().map(|d| d.join("bin").join(name));
    let beside_exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name)));
    [from_resources, beside_exe].into_iter().flatten().find(|p| p.is_file())
}

/// 设置页要显示的整体状态。一次取回，省得前端串三个命令。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    /// 自带 CLI 的路径；`None` = 这个构建里没带（例如直接 cargo run）
    pub source: Option<String>,
    /// `installed` / `foreign` / `missing`
    pub link_state: String,
    /// 已装时是软链的位置；未装时是**建议**的落点
    pub link: Option<String>,
    /// 落点不在 PATH 里，装完还要用户自己加进去
    pub needs_path_setup: bool,
    /// 当前工作空间里 apicase 段落的状态：`ready` / `stale` / `absent`。
    /// **三态而非有/无**——段落在但内容是旧版本时报 `stale`，否则用户看着「已配置」，
    /// AI 手上却是上个版本的说明。
    pub agents_state: String,
}

#[tauri::command]
pub fn ai_status(app: AppHandle, workspace: Option<String>) -> AiStatus {
    let source = bundled_cli(&app);
    let (link_state, link, needs_path_setup) = match source.as_deref().map(cli_link::status) {
        Some(cli_link::LinkStatus::Installed { link }) => ("installed", Some(link), false),
        Some(cli_link::LinkStatus::Foreign { link }) => ("foreign", Some(link), false),
        Some(cli_link::LinkStatus::Missing { target, needs_path_setup }) => {
            ("missing", target, needs_path_setup)
        }
        // 构建里没带 CLI：状态就是「装不了」，前端据此把按钮灰掉
        None => ("unavailable", None, false),
    };
    // 段落正文里有没有那段兜底路径，取决于此刻 CLI 在不在 PATH——判定与生成必须同一个前提，
    // 否则装完 CLI 会凭空多出一次「不一致」
    let on_path = link_state == "installed";
    let agents_state = match workspace.as_deref().map(std::path::Path::new) {
        Some(ws) => match agents::state(ws, on_path) {
            agents::State::Ready => "ready",
            agents::State::Stale => "stale",
            agents::State::Absent => "absent",
        },
        None => "absent",
    };
    AiStatus {
        source: source.map(|p| p.to_string_lossy().into_owned()),
        link_state: link_state.into(),
        link: link.map(|p| p.to_string_lossy().into_owned()),
        needs_path_setup,
        agents_state: agents_state.into(),
    }
}

/// 装进 PATH。错误原样返回给前端显示——「被别的 apicase 占用」「目录不可写」
/// 这些都是用户能自己处理的，不该被吞成一句「失败」。
#[tauri::command]
pub fn ai_install_cli(app: AppHandle) -> Result<AiStatus, String> {
    let source = bundled_cli(&app).ok_or("这个构建里没有自带命令行工具")?;
    cli_link::install(&source)?;
    Ok(ai_status(app, None))
}

// 没有对应的「移除」命令：设置页只提供「初始化」这一个动作（用户要的是「能用」），
// 而卸载是一次性的收尾操作，`apicase self uninstall` 已经完整覆盖——
// 在两处各留一份实现，迟早会有一处忘了跟。

/// 往工作空间写 `AGENTS.md`。**幂等**，且绝不覆盖用户自己写的内容。
///
/// `cli_on_path` 由 core 现算而不是让前端传：那是「此刻敲不敲得到 apicase」的事实，
/// 前端手里的可能是几秒前的旧状态，而它决定了要不要在文件里附一段兜底路径。
#[tauri::command]
pub fn ai_write_agents(app: AppHandle, workspace: String) -> Result<String, String> {
    let on_path = bundled_cli(&app)
        .map(|s| matches!(cli_link::status(&s), cli_link::LinkStatus::Installed { .. }))
        .unwrap_or(false);
    let how = agents::write(std::path::Path::new(&workspace), on_path)?;
    Ok(match how {
        agents::Written::Created => "已生成 AGENTS.md",
        agents::Written::Appended => "已在 AGENTS.md 中追加 apicase 段落",
        agents::Written::Updated => "已更新 AGENTS.md 中的 apicase 段落",
        agents::Written::Unchanged => "AGENTS.md 已是最新",
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态里的字符串是**前端的判断依据**，改了就静默错位（按钮显示成反的）
    #[test]
    fn link_states_are_stable_strings() {
        for s in ["installed", "foreign", "missing", "unavailable"] {
            assert!(!s.is_empty());
        }
        // 序列化形状：前端读的是 camelCase
        let st = AiStatus {
            source: Some("/x/apicase".into()),
            link_state: "missing".into(),
            link: Some("/y/apicase".into()),
            needs_path_setup: true,
            agents_state: "stale".into(),
        };
        let v = serde_json::to_value(&st).expect("序列化");
        assert_eq!(v["linkState"], "missing");
        assert_eq!(v["needsPathSetup"], true);
        assert_eq!(v["agentsState"], "stale");
    }

    /// AGENTS.md 的三态同样是前端的判断依据
    #[test]
    fn agents_states_are_stable_strings() {
        for s in ["ready", "stale", "absent"] {
            assert!(!s.is_empty());
        }
    }
}
