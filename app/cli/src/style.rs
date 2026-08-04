//! 终端着色与对齐。
//!
//! 自己写 ANSI 而不引配色库：需要的只是七八个颜色和一条「什么时候不着色」的规则。
//! 后者才是真正要想清楚的部分，而它不在任何库里。

use crate::cli::ColorWhen;
use std::io::IsTerminal;

/// 何时着色的判定结果，连同一组语义化的着色方法。
#[derive(Debug, Clone, Copy)]
pub struct Style {
    on: bool,
}

impl Style {
    /// 决定要不要着色。三条规则依次生效：
    ///
    /// 1. `--color always|never` 显式指定的最大。
    /// 2. [`NO_COLOR`](https://no-color.org/) 环境变量存在且非空 → 不着色。这是跨语言的行业约定，
    ///    用户在自己的 shell 里设一次就该对所有工具生效。
    /// 3. 否则跟着「输出是不是终端」走——重定向进文件或管道时着色只会变成一堆 `\x1b[32m`。
    pub fn resolve(when: ColorWhen, is_tty: bool) -> Self {
        let on = match when {
            ColorWhen::Always => true,
            ColorWhen::Never => false,
            ColorWhen::Auto => is_tty && !no_color_env(),
        };
        Self { on }
    }

    /// 按 stdout 是不是终端来决定。
    pub fn for_stdout(when: ColorWhen) -> Self {
        Self::resolve(when, std::io::stdout().is_terminal())
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn pass(&self, s: &str) -> String {
        self.paint("32", s) // 绿
    }
    pub fn fail(&self, s: &str) -> String {
        self.paint("31", s) // 红
    }
    pub fn error(&self, s: &str) -> String {
        self.paint("35", s) // 品红：与断言失败的红分开——两者的排查方向完全不同
    }
    pub fn skip(&self, s: &str) -> String {
        self.paint("90", s) // 亮黑（灰）
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn warn(&self, s: &str) -> String {
        self.paint("33", s) // 黄
    }
}

fn no_color_env() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// 字符串在等宽终端里占几列。
///
/// 用例路径里常有中文目录名（`01-登录/…`），按字符数补空格会让整列歪掉。
/// 这里只区分「东亚宽字符算 2 列、其余算 1 列」——不引 unicode-width：
/// 那个 crate 处理的组合字符、emoji 变体选择符等情形，在文件路径里几乎不会出现，
/// 而判错的代价只是某一行多 / 少一个空格。
pub fn width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    let c = c as u32;
    let wide = matches!(c,
        0x1100..=0x115F        // 韩文字母
        | 0x2E80..=0x303E      // CJK 部首、假名标点
        | 0x3041..=0x33FF      // 平假名、片假名、注音、CJK 兼容
        | 0x3400..=0x4DBF      // CJK 扩展 A
        | 0x4E00..=0x9FFF      // CJK 统一表意
        | 0xA000..=0xA4CF      // 彝文
        | 0xAC00..=0xD7A3      // 韩文音节
        | 0xF900..=0xFAFF      // CJK 兼容表意
        | 0xFE30..=0xFE6F      // CJK 兼容形式
        | 0xFF00..=0xFF60      // 全角 ASCII
        | 0xFFE0..=0xFFE6      // 全角符号
        | 0x1F300..=0x1F64F    // 表情与符号
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD    // CJK 扩展 B 及以后
    );
    if wide {
        2
    } else {
        1
    }
}

/// 左对齐补到 `w` 列（按显示宽度）。已经超宽则原样返回，不截断——
/// 路径被截掉尾巴比列没对齐更难用。
pub fn pad(s: &str, w: usize) -> String {
    let n = width(s);
    if n >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_follows_the_flag_then_the_terminal() {
        assert!(Style::resolve(ColorWhen::Always, false).on, "显式 always 压过非终端");
        assert!(!Style::resolve(ColorWhen::Never, true).on);
        assert!(Style::resolve(ColorWhen::Auto, true).on || no_color_env());
        assert!(!Style::resolve(ColorWhen::Auto, false).on, "管道里不着色");
    }

    #[test]
    fn painting_is_a_noop_when_off() {
        let off = Style { on: false };
        assert_eq!(off.pass("✓"), "✓");
        let on = Style { on: true };
        assert_eq!(on.pass("✓"), "\x1b[32m✓\x1b[0m");
    }

    /// 中文按 2 列算，否则含中文目录名的那几行会整列歪掉
    #[test]
    fn width_counts_east_asian_chars_as_two_columns() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("登录"), 4);
        assert_eq!(width("01-登录/a.yml"), 3 + 4 + 6);
        assert_eq!(pad("登录", 6), "登录  ");
        assert_eq!(pad("abcdef", 3), "abcdef", "超宽不截断");
    }
}
