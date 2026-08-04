//! 校验用例：只解析、不发请求。
//!
//! **这是给 AI 的自检入口**，也是人写完一批用例后的第一道关。它比 `analyze_case`
//! 多管一层：那个只回答「结构化编辑器能不能打开它」，这里还要回答
//! 「照这么写跑起来会不会是白跑」——依赖指向不存在的 step、断言目标写成认不出的形式，
//! 都能解析、都能跑，但结果注定没有意义。这类问题在运行报告里表现为一堆
//! 「实际值 ∅」，看的人未必想得到根因在配置本身。

use super::read_text;
use apicase_core::model::Case;
use apicase_core::workspace::Workspace;
use apicase_core::{assert, discover, yaml};
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;

/// 问题的轻重。
///
/// - `error`：跑起来必然不对（解析失败、依赖断裂、URL 空）。
/// - `warning`：跑得动但很可能不是本意（断言目标认不出、断言缺期望值）。
///
/// 分级不是为了好看：CI 里 `check` 的退出码只看 error，warning 不该把流水线卡住，
/// 但要显示出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub severity: Severity,
    /// 出问题的位置，形如 `steps[2].assertions[0]`——只说"断言目标不对"
    /// 而不说是哪一条，用户得自己在几十条里找
    pub at: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub file: String,
    /// 没有 error 级问题即为 true（warning 不影响）
    pub ok: bool,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
    pub results: Vec<CheckResult>,
    pub total: usize,
    pub ok: usize,
    pub errors: usize,
    pub warnings: usize,
}

impl CheckReport {
    /// 单个结果也包成报告——`check -` 走内联文本，与目录校验共用同一套输出与退出码。
    pub fn of_one(result: CheckResult) -> Self {
        Self::of(vec![result])
    }

    fn of(results: Vec<CheckResult>) -> Self {
        let total = results.len();
        let ok = results.iter().filter(|r| r.ok).count();
        let errors = results.iter().flat_map(|r| &r.issues).filter(|i| i.severity == Severity::Error).count();
        let warnings =
            results.iter().flat_map(|r| &r.issues).filter(|i| i.severity == Severity::Warning).count();
        Self { results, total, ok, errors, warnings }
    }
}

/// 校验目标下的全部用例。
pub fn check(ws: &Workspace, targets: &[PathBuf], recursive: bool) -> CheckReport {
    let roots: Vec<PathBuf> = if targets.is_empty() { vec![ws.root.clone()] } else { targets.to_vec() };
    let results = discover::find_all(&roots, recursive)
        .into_iter()
        .map(|p| match read_text(&p) {
            Ok(text) => check_text(&text, &ws.rel(&p)),
            Err(e) => CheckResult {
                file: ws.rel(&p),
                ok: false,
                issues: vec![Issue { severity: Severity::Error, at: String::new(), message: e }],
            },
        })
        .collect();
    CheckReport::of(results)
}

/// 校验一段 case 文本。`file` 只用于回报，不读盘——MCP 的 `content` 参数走这条。
pub fn check_text(text: &str, file: &str) -> CheckResult {
    let analyzed = yaml::analyze_case(text);
    let Some(case) = analyzed.case.filter(|_| analyzed.valid) else {
        return CheckResult {
            file: file.to_string(),
            ok: false,
            issues: vec![Issue {
                severity: Severity::Error,
                at: String::new(),
                message: analyzed.error.unwrap_or_else(|| "不是有效的用例".into()),
            }],
        };
    };

    let issues = inspect(&case);
    CheckResult {
        ok: !issues.iter().any(|i| i.severity == Severity::Error),
        issues,
        file: file.to_string(),
    }
}

fn inspect(case: &Case) -> Vec<Issue> {
    let mut out = Vec::new();
    let err = |at: String, message: String| Issue { severity: Severity::Error, at, message };
    let warn = |at: String, message: String| Issue { severity: Severity::Warning, at, message };

    if case.requests.is_empty() {
        out.push(err(String::new(), "没有任何请求（steps 是空的）".into()));
        return out;
    }

    let ids: HashSet<&str> = case.requests.iter().map(|s| s.id.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();

    for (i, s) in case.requests.iter().enumerate() {
        let at = |suffix: &str| format!("steps[{i}]{suffix}");

        // id 重复 = ${{steps.<id>.outputs.*}} 与 dependsOn 都会指向"其中一个"，
        // 而具体是哪一个取决于遍历顺序——这种不确定性必须挡在运行之前
        if s.id.trim().is_empty() {
            out.push(err(at(""), "请求缺少 id".into()));
        } else if !seen.insert(s.id.as_str()) {
            out.push(err(at(""), format!("请求 id 重复：{}", s.id)));
        }

        if s.http.url.trim().is_empty() {
            out.push(err(at(".request.url"), "URL 是空的".into()));
        }

        for (j, dep) in s.depends_on.iter().enumerate() {
            if !ids.contains(dep.as_str()) {
                out.push(err(
                    at(&format!(".dependsOn[{j}]")),
                    format!("依赖的请求不存在：{dep}"),
                ));
            }
        }

        for (j, a) in s.assertions.iter().enumerate() {
            let at = at(&format!(".assertions[{j}]"));
            if a.target.trim().is_empty() {
                out.push(warn(at.clone(), "断言没有目标，运行时会被跳过".into()));
            } else if !assert::is_known_target(&a.target) {
                out.push(warn(
                    at.clone(),
                    format!(
                        "认不出的断言目标 `{}`——目标要写成 res.status / res.headers.<名> / res.body<路径>",
                        a.target.trim()
                    ),
                ));
            }
            if a.op.needs_value() && a.value.as_deref().unwrap_or("").is_empty() {
                out.push(warn(at, format!("{} 断言没有期望值", a.op.as_str())));
            }
        }

        for (j, o) in s.outputs.iter().enumerate() {
            let at = at(&format!(".outputs[{j}]"));
            if o.name.trim().is_empty() {
                out.push(warn(at.clone(), "输出没有变量名，运行时会被跳过".into()));
            }
            if !o.path.trim().is_empty() && !assert::is_known_target(&o.path) {
                out.push(warn(at, format!("认不出的提取路径 `{}`——写法同断言目标", o.path.trim())));
            }
        }
    }

    // 成环：core 的 topo_order 遇到环会按出现序兜底跑完（不死循环），
    // 但那时的执行顺序已经不是用户以为的那个了，必须报出来
    if let Some(cycle) = find_cycle(case) {
        out.push(err(String::new(), format!("依赖成环：{cycle}")));
    }
    out
}

/// 找一条依赖环并拼成 `a → b → a`。只报第一条——环通常是同一处笔误的连带结果，
/// 把它改了再跑一次比一次列出五条更省事。
fn find_cycle(case: &Case) -> Option<String> {
    use std::collections::HashMap;
    let idx: HashMap<&str, usize> =
        case.requests.iter().enumerate().map(|(i, s)| (s.id.as_str(), i)).collect();
    let n = case.requests.len();
    // 0 = 没访问过，1 = 在当前递归栈上，2 = 已确认无环
    let mut state = vec![0u8; n];
    let mut path: Vec<usize> = Vec::new();

    fn dfs(
        i: usize,
        case: &Case,
        idx: &HashMap<&str, usize>,
        state: &mut [u8],
        path: &mut Vec<usize>,
    ) -> Option<String> {
        state[i] = 1;
        path.push(i);
        for dep in &case.requests[i].depends_on {
            let Some(&j) = idx.get(dep.as_str()) else { continue };
            if state[j] == 1 {
                let from = path.iter().position(|&k| k == j).unwrap_or(0);
                let mut names: Vec<&str> = path[from..].iter().map(|&k| case.requests[k].id.as_str()).collect();
                names.push(case.requests[j].id.as_str());
                return Some(names.join(" → "));
            }
            if state[j] == 0 {
                if let Some(c) = dfs(j, case, idx, state, path) {
                    return Some(c);
                }
            }
        }
        path.pop();
        state[i] = 2;
        None
    }

    for i in 0..n {
        if state[i] == 0 {
            if let Some(c) = dfs(i, case, &idx, &mut state, &mut path) {
                return Some(c);
            }
            path.clear();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues_of(yaml: &str) -> Vec<Issue> {
        check_text(yaml, "t.yml").issues
    }

    #[test]
    fn clean_case_has_no_issues() {
        let r = check_text(
            "apicase: v0.1\nsteps:\n  - id: a\n    request:\n      method: GET\n      url: http://x/a\n\
             \n    assertions:\n      - { target: res.status, op: eq, value: 200 }\n",
            "t.yml",
        );
        assert!(r.ok, "{:?}", r.issues);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
    }

    #[test]
    fn broken_yaml_is_a_single_error() {
        let r = check_text("这不是: [有效\n  yaml", "t.yml");
        assert!(!r.ok);
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.issues[0].severity, Severity::Error);
    }

    #[test]
    fn dangling_dependency_and_duplicate_id_are_errors() {
        let is = issues_of(
            "apicase: v0.1\nsteps:\n  - id: a\n    request: { url: http://x }\n\
             \n  - id: a\n    dependsOn: [幽灵]\n    request: { url: http://x }\n",
        );
        let msgs: Vec<&str> = is.iter().map(|i| i.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("id 重复")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("依赖的请求不存在")), "{msgs:?}");
        assert!(is.iter().all(|i| i.severity == Severity::Error));
        assert_eq!(is[1].at, "steps[1].dependsOn[0]", "位置要精确到那一条");
    }

    /// 认不出的断言目标运行时恒取不到值——跑得动，但结果注定没意义
    #[test]
    fn unknown_assert_target_is_a_warning() {
        let is = issues_of(
            "apicase: v0.1\nsteps:\n  - id: a\n    request: { url: http://x }\n\
             \n    assertions:\n      - { target: status, op: eq, value: 200 }\n\
             \n      - { target: res.bodyfoo, op: eq, value: 1 }\n",
        );
        assert_eq!(is.len(), 2, "{is:?}");
        assert!(is.iter().all(|i| i.severity == Severity::Warning));
        assert!(is[0].message.contains("认不出的断言目标"));
        assert!(check_text(
            "apicase: v0.1\nsteps:\n  - id: a\n    request: { url: http://x }\n\
             \n    assertions:\n      - { target: status, op: eq, value: 200 }\n",
            "t.yml"
        )
        .ok, "只有 warning 时 ok 仍为 true");
    }

    #[test]
    fn empty_url_is_an_error() {
        let is = issues_of("apicase: v0.1\nsteps:\n  - id: a\n    request: { method: GET }\n");
        assert_eq!(is.len(), 1);
        assert_eq!(is[0].severity, Severity::Error);
        assert_eq!(is[0].at, "steps[0].request.url");
    }

    /// 成环时 core 会按出现序兜底跑完，但那已不是用户以为的顺序
    #[test]
    fn cycles_are_reported_once_with_the_path() {
        let is = issues_of(
            "apicase: v0.1\nsteps:\n  - id: a\n    dependsOn: [b]\n    request: { url: http://x }\n\
             \n  - id: b\n    dependsOn: [a]\n    request: { url: http://x }\n",
        );
        let cycles: Vec<&Issue> = is.iter().filter(|i| i.message.starts_with("依赖成环")).collect();
        assert_eq!(cycles.len(), 1, "只报第一条：{is:?}");
        assert!(cycles[0].message.contains("→"), "{}", cycles[0].message);
    }

    #[test]
    fn empty_steps_short_circuits() {
        let r = check_text("apicase: v0.1\nsteps: []\n", "t.yml");
        assert!(!r.ok);
        assert_eq!(r.issues.len(), 1, "空用例不必再往下查每个字段");
    }
}
