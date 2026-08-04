//! 列出用例：路径、名称、每个 step 的方法与 URL。
//!
//! 定位是「让人和 AI 一眼看清这个工作空间里有什么」，因此**每条都要有摘要**——
//! 只给一列文件名的话，找"登录接口在哪个文件"仍得逐个打开。

use super::read_text;
use apicase_core::discover;
use apicase_core::workspace::Workspace;
use apicase_core::yaml;
use serde::Serialize;
use std::path::PathBuf;

/// 一个 step 的概要。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepBrief {
    pub id: String,
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    pub assertions: usize,
}

/// 一个用例文件。
///
/// **解析失败的也列出来**（`valid: false` + 原因）：一个语法写坏的用例在清单里
/// 悄悄消失，会让人以为它根本不存在，而它明明躺在目录里、还会被 `run` 算作跳过。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItem {
    /// 相对工作空间根，`/` 分隔
    pub file: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub steps: Vec<StepBrief>,
}

/// 列出目标下的用例。`query` 非空时按「文件路径 / case 名 / step 的 URL」子串匹配
/// （大小写不敏感）——找一个接口时，记得住的可能是路径、名字，也可能是那段 URL。
pub fn list(ws: &Workspace, targets: &[PathBuf], recursive: bool, query: Option<&str>) -> Vec<ListItem> {
    let roots: Vec<PathBuf> = if targets.is_empty() { vec![ws.root.clone()] } else { targets.to_vec() };
    let q = query.map(str::to_lowercase).filter(|s| !s.is_empty());

    discover::find_all(&roots, recursive)
        .into_iter()
        .map(|p| item_of(ws, &p))
        .filter(|it| q.as_deref().is_none_or(|q| matches(it, q)))
        .collect()
}

fn matches(it: &ListItem, q: &str) -> bool {
    it.file.to_lowercase().contains(q)
        || it.name.as_deref().is_some_and(|n| n.to_lowercase().contains(q))
        || it.steps.iter().any(|s| s.url.to_lowercase().contains(q) || s.id.to_lowercase().contains(q))
}

fn item_of(ws: &Workspace, path: &std::path::Path) -> ListItem {
    let file = ws.rel(path);
    let path_s = path.to_string_lossy().into_owned();
    let text = match read_text(path) {
        Ok(t) => t,
        Err(e) => {
            return ListItem { file, path: path_s, name: None, valid: false, error: Some(e), steps: Vec::new() }
        }
    };

    let analyzed = yaml::analyze_case(&text);
    match analyzed.case.filter(|_| analyzed.valid) {
        Some(c) => ListItem {
            file,
            path: path_s,
            name: c.name.filter(|n| !n.is_empty()),
            valid: true,
            error: None,
            steps: c
                .requests
                .iter()
                .map(|s| StepBrief {
                    id: s.id.clone(),
                    method: s.http.method.clone(),
                    url: s.http.url.clone(),
                    depends_on: s.depends_on.clone(),
                    assertions: s.assertions.len(),
                })
                .collect(),
        },
        None => ListItem {
            file,
            path: path_s,
            name: None,
            valid: false,
            error: Some(analyzed.error.unwrap_or_else(|| "不是有效的用例".into())),
            steps: Vec::new(),
        },
    }
}
