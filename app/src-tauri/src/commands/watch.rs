//! 工作空间的文件系统监听。
//!
//! 外部改动（编辑器保存、git checkout、另一个 apicase 窗口）要能实时反映到文件树，
//! 否则用户看到的是一份过期快照。跨平台原生后端：macOS FSEvents / Linux inotify /
//! Windows ReadDirectoryChangesW，由 `notify` crate 统一。

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

/// 文件监听的托管状态：持有当前 watcher（drop 即停止监听）。
#[derive(Default)]
pub struct WatchState(Mutex<Option<RecommendedWatcher>>);

/// 是否为应忽略的噪声路径。
///
/// **不按"隐藏项"一刀切**：文件树有「显示隐藏文件」开关，开着时 `.apicase/`、`.env`、
/// `.gitignore` 都是**可见的**——把它们的变更滤掉，用户删掉一份报告后文件树仍显示它，
/// 看到的是一份过期快照（这正是监听要解决的问题）。前端只刷新**已加载**的目录，
/// 没展开过的隐藏目录本来就不会因此空转，Rust 侧没有再挡一道的必要。
///
/// 真正要挡的是**高频且与用例无关**的目录：`.git`（一次 checkout / index 更新就是成百上千个
/// 事件）与三个构建产物目录。
fn is_noise_path(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == ".git" || s == "node_modules" || s == "target" || s == "dist"
    })
}

/// 把一个事件的有效路径并入本批（跳过纯访问事件与噪声路径）。
fn collect_paths(batch: &mut HashSet<String>, ev: notify::Event) {
    if matches!(ev.kind, EventKind::Access(_)) {
        return; // 打开/读取等访问事件不改变内容，忽略
    }
    for p in ev.paths {
        if is_noise_path(&p) {
            continue;
        }
        batch.insert(p.to_string_lossy().to_string());
    }
}

/// 有变更则把受影响路径列表通过事件发往前端。
fn emit_changes(app: &AppHandle, batch: &HashSet<String>) {
    if batch.is_empty() {
        return;
    }
    let paths: Vec<String> = batch.iter().cloned().collect();
    let _ = app.emit("workspace:fs-change", paths);
}

/// Tauri 命令：监听工作空间目录的文件系统变更（创建/修改/删除/重命名）。
/// 事件经 250ms 去抖后，以受影响路径列表通过 `workspace:fs-change` 发往前端。
/// 再次调用会替换旧监听（切换工作空间时用）。
#[tauri::command]
pub fn watch_workspace(
    app: AppHandle,
    state: State<WatchState>,
    path: String,
) -> Result<(), String> {
    let root = Path::new(&path);
    if !root.is_dir() {
        return Err(format!("不是目录: {path}"));
    }
    let (tx, rx) = channel::<notify::Event>();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| format!("创建文件监听失败: {e}"))?;
    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| format!("启动文件监听失败: {e}"))?;

    // 去抖批处理线程：收敛突发事件后成批上报。
    // 当 watcher 被替换/丢弃时，tx 随其闭包销毁 → rx 断开 → 本线程退出。
    // rx.recv() 返回 Err 即 watcher 已被替换 / 丢弃，循环随之结束
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut batch: HashSet<String> = HashSet::new();
            collect_paths(&mut batch, first);
            loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(ev) => collect_paths(&mut batch, ev),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        emit_changes(&app, &batch);
                        return;
                    }
                }
            }
            emit_changes(&app, &batch);
        }
    });

    // 替换旧 watcher（drop 旧值即停止其监听与批处理线程）
    *state.0.lock().unwrap() = Some(watcher);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 隐藏目录下的变更**必须**上报。这条规则原先按"`.` 开头即噪声"一刀切，
    /// 结果删掉 `.apicase/` 下的报告后文件树仍显示它——正是监听要解决的问题。
    #[test]
    fn hidden_paths_are_not_noise() {
        for p in ["/ws/.apicase/reports/20260728-215758-01-方法.html", "/ws/.env", "/ws/.gitignore"] {
            assert!(!is_noise_path(Path::new(p)), "{p} 不该被当噪声滤掉");
        }
    }

    /// 挡住的是高频且与用例无关的目录
    #[test]
    fn git_and_build_dirs_are_noise() {
        for p in ["/ws/.git/index", "/ws/.git/objects/ab/cdef", "/ws/node_modules/x/y.js", "/ws/target/debug/f", "/ws/dist/b.js"] {
            assert!(is_noise_path(Path::new(p)), "{p} 应被滤掉");
        }
    }

    /// 按**完整路径段**比对，名字相近的正常文件不该被误伤
    #[test]
    fn similar_names_are_not_matched() {
        for p in ["/ws/.github/workflows/ci.yml", "/ws/distributed.yml", "/ws/my-target/a.yml", "/ws/a.git.yml"] {
            assert!(!is_noise_path(Path::new(p)), "{p} 不该被误当噪声");
        }
    }
}
