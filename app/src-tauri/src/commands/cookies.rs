//! Cookie jar 的管理命令（设置页「Cookies」用）。
//!
//! 同 `exec`：**没有语义**，全部转发给 `apicase-core` 的 `cookie` 模块。
//! jar 的路径由前端给（`<workspace>/.apicase/cookies.yml`）——core 不猜工作空间在哪，
//! 将来的 CLI 会用同一批函数、自己决定路径。

use apicase_core::cookie::{self, CookieInput, CookieItem, CookieKey};

/// 读回一份 jar 里的全部 cookie（按 域 → 路径 → 名 排序，含会话与已过期的）。
#[tauri::command]
pub fn list_cookies(jar_path: String) -> Vec<CookieItem> {
    cookie::jar_at(Some(&jar_path)).list()
}

/// 新增或修改一条。`prev` 是修改前的主键（改了域 / 路径 / 名就得删掉原来那条）；
/// 新增时传 null。校验不过时返回可直接展示给用户的中文错误。
#[tauri::command]
pub fn save_cookie(jar_path: String, prev: Option<CookieKey>, cookie: CookieInput) -> Result<(), String> {
    cookie::jar_at(Some(&jar_path)).put(prev.as_ref(), &cookie)
}

/// 删一条。`domain + path + name` 是 cookie 的主键——只给 name 会误删同名不同域的那条。
#[tauri::command]
pub fn delete_cookie(jar_path: String, domain: String, path: String, name: String) -> bool {
    cookie::jar_at(Some(&jar_path)).remove(&domain, &path, &name)
}

/// 清空：`domain` 给了就只清该域，否则全清。返回清掉的条数。
#[tauri::command]
pub fn clear_cookies(jar_path: String, domain: Option<String>) -> usize {
    cookie::jar_at(Some(&jar_path)).clear(domain.as_deref())
}
