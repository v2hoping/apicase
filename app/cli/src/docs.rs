//! 把 case YAML 格式规范喂给 AI。
//!
//! # 为什么这是 MCP 里最重要的一个工具
//!
//! AI 不知道 apicase 的 YAML schema，靠猜必然写错——写出 Postman 风格的 JSON、
//! 写出 `{{var}}` 而不是 `${{var}}`、把断言目标写成 `$.data.token`。
//! 这些错误跑起来才发现，来回好几轮。先让它读一遍规范，一次就能写对。
//!
//! # 单一事实源
//!
//! 内容用 `include_str!` 直接嵌 `docs/0.latest/3.YAML格式规范.md`，**不另写一份速查**：
//! 另写一份的那一刻就有了两个真相，而漂移是必然的（规范改了、速查没改，AI 照着旧的写）。
//! 代价是按标题切分要认得那份文档的结构，换来的是零同步成本。

/// 格式规范全文。**编译期嵌入**——CLI 是单个可执行文件，跑在没有仓库的机器上。
const SPEC: &str = include_str!("../../../docs/0.latest/3.YAML格式规范.md");

/// 一个可请求的主题：名字 → 要摘出的标题。
///
/// 分主题不是为了好看，是因为**上下文是 AI 最稀缺的资源**：整份规范一万五千字，
/// 而「我要加个认证」这个问题只需要其中三十行。
pub struct Topic {
    pub name: &'static str,
    pub about: &'static str,
    /// 要摘的标题（按标题文本的前缀匹配），空 = 全文
    headings: &'static [&'static str],
}

pub const TOPICS: &[Topic] = &[
    Topic {
        name: "case",
        about: "用例的整体结构：顶层字段与 step（默认）",
        headings: &["总览", "顶层字段", "step（请求节点）", "request（http 报文）"],
    },
    Topic { name: "assertions", about: "断言与输出提取的写法", headings: &["outputs（输出提取）", "assertions（断言）"] },
    Topic { name: "auth", about: "六种认证方式的配置", headings: &["auth（认证）"] },
    Topic { name: "body", about: "请求体的七种类型", headings: &["body（请求体）"] },
    Topic { name: "vars", about: "变量语法与优先级", headings: &["变量与透传"] },
    Topic {
        name: "settings",
        about: "application.yml：多套环境与工作空间设置",
        headings: &["application.yml（工作空间配置）"],
    },
    Topic {
        name: "cookies",
        about: ".apicase/cookies.yml：cookie 会话文件的格式（直接编辑它，没有对应的命令）",
        headings: &["cookies.yml（cookie 会话）"],
    },
    Topic { name: "all", about: "规范全文", headings: &[] },
];

/// 取一个主题的内容。认不出的主题名回落到 `case`，并在开头说明——
/// 报错让 AI 多跑一轮，而它真正想要的多半就是用例怎么写。
pub fn topic(name: Option<&str>) -> String {
    let want = name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("case");
    let Some(t) = TOPICS.iter().find(|t| t.name.eq_ignore_ascii_case(want)) else {
        return format!(
            "（没有名为 `{want}` 的主题，以下是 `case`。可选主题：{}）\n\n{}",
            TOPICS.iter().map(|t| t.name).collect::<Vec<_>>().join(" / "),
            sections(TOPICS[0].headings)
        );
    };
    if t.headings.is_empty() {
        return SPEC.to_string();
    }
    sections(t.headings)
}

/// 主题清单，给 `apicase_docs` 的工具描述与 `--help` 用。
pub fn topic_list() -> String {
    TOPICS.iter().map(|t| format!("{}（{}）", t.name, t.about)).collect::<Vec<_>>().join("、")
}

/// 按标题摘出若干节。
///
/// 一节 = 从匹配的标题行起，到**下一个同级或更高级**的标题为止。
/// 按级别而不是按「下一个 `#`」切，是因为 `## step` 底下还有 `### request` 这些子节，
/// 按后者切会把子节全丢掉。
fn sections(headings: &[&str]) -> String {
    let lines: Vec<&str> = SPEC.lines().collect();
    let mut out = String::new();
    for want in headings {
        let Some(start) = lines.iter().position(|l| heading_matches(l, want)) else {
            continue;
        };
        let level = heading_level(lines[start]);
        let end = lines[start + 1..]
            .iter()
            .position(|l| heading_level(l).is_some_and(|n| n <= level.unwrap_or(9)))
            .map(|i| start + 1 + i)
            .unwrap_or(lines.len());
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&lines[start..end].join("\n"));
        out.push('\n');
    }
    if out.is_empty() {
        // 文档结构变了而这里没跟上——给全文而不是给空白，功能降级但不失效
        return SPEC.to_string();
    }
    out
}

fn heading_level(line: &str) -> Option<usize> {
    let n = line.bytes().take_while(|b| *b == b'#').count();
    (n > 0 && line.as_bytes().get(n) == Some(&b' ')).then_some(n)
}

fn heading_matches(line: &str, want: &str) -> bool {
    heading_level(line).is_some_and(|n| line[n..].trim() == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个主题都要真的摘到东西。这条测试同时是**文档结构的护栏**：
    /// 规范里的标题被改名了，这里立刻红——否则 AI 拿到的是悄悄退化成全文的内容。
    #[test]
    fn every_topic_resolves_to_its_own_section() {
        for t in TOPICS {
            let text = topic(Some(t.name));
            assert!(!text.trim().is_empty(), "主题 {} 是空的", t.name);
            if t.headings.is_empty() {
                continue;
            }
            for h in t.headings {
                assert!(text.contains(h), "主题 {} 应含标题「{h}」", t.name);
            }
            assert!(text.len() < SPEC.len(), "主题 {} 退化成了全文（标题多半改名了）", t.name);
        }
    }

    #[test]
    fn case_topic_is_the_default_and_covers_the_essentials() {
        let d = topic(None);
        assert_eq!(d, topic(Some("case")));
        // 写一条用例最少要知道的四件事
        for must in ["apicase: v0.1", "steps:", "dependsOn", "${{baseUrl}}"] {
            assert!(d.contains(must), "默认主题里应有 `{must}`");
        }
    }

    /// 认不出的主题名要给出可选项，而不是让 AI 空手而归
    #[test]
    fn unknown_topic_falls_back_with_an_explanation() {
        let t = topic(Some("没这个"));
        assert!(t.starts_with("（没有名为 `没这个` 的主题"), "{}", &t[..60.min(t.len())]);
        assert!(t.contains("assertions"), "要列出可选主题");
        assert!(t.contains("steps:"), "回落的内容仍要有用");
    }

    /// 一节要连它的子节一起摘走：`## step` 底下的 `### request` 不能丢
    #[test]
    fn a_section_carries_its_subsections() {
        let s = sections(&["step（请求节点）"]);
        assert!(s.contains("### request"), "子节要跟着走：{s}");
        assert!(!s.contains("## 变量与透传"), "不该越过下一个同级标题");
    }

    #[test]
    fn heading_level_only_counts_real_headings() {
        assert_eq!(heading_level("## 总览"), Some(2));
        assert_eq!(heading_level("### a"), Some(3));
        assert_eq!(heading_level("#没空格"), None);
        assert_eq!(heading_level("普通行"), None);
    }
}
