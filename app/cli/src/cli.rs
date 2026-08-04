//! 命令行的形状：子命令、选项、默认值。
//!
//! 用 `clap` 的 derive API 而不是手写解析：子命令、短长选项、`--help` 排版、
//! shell 补全生成这几样手写很快会失控，而「遵循业界规范」正是 CLI 的核心诉求。
//! 这一层**只描述形状**，不含任何逻辑——逻辑在 `ops`，呈现在 `render`。

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// 帮助排版模板。`{usage}` 产出的仍是 `apicase run [OPTIONS] [目标]...`——
/// 方括号里的是**占位符不是文案**，各家 CLI 一律英文，翻译反而不标准了。
const HELP_TEMPLATE: &str = "\
{about-with-newline}
用法：{usage}

{all-args}{after-help}";

/// 解析命令行。
///
/// 不直接 `Cli::parse()`，是为了把排版模板与「选项」这个分组标题**传播到每个子命令**：
/// derive 的 `#[command(help_template = …)]` 只作用于它所在的那一层，
/// 而 `apicase run --help` 是比顶层帮助更常被读到的一屏。
pub fn parse() -> Cli {
    let matches = localize(Cli::command()).get_matches();
    // 走到这里说明解析成功；失败时 clap 已自己打了错误并退出
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
}

fn localize(cmd: clap::Command) -> clap::Command {
    let names: Vec<String> = cmd.get_subcommands().map(|s| s.get_name().to_string()).collect();
    let mut cmd = cmd.help_template(HELP_TEMPLATE).next_help_heading("选项");
    for name in names {
        // 递归：`apicase env show --help` 也该是同一套排版
        cmd = cmd.mut_subcommand(name, localize);
    }
    cmd
}

#[derive(Parser, Debug)]
#[command(
    name = "apicase",
    version,
    about = "apicase —— API 调试、管理与用例编排",
    long_about = "apicase —— API 调试、管理与用例编排\n\n\
用例是 YAML 文本，直接用编辑器改即可；本命令负责运行、校验与查看。\n\
`apicase mcp` 以 MCP 服务器运行，供 AI Agent 调用。\n\
图形界面是独立的程序（apicase-desktop），与本命令共用同一个执行内核。",
    // 无参数时打印帮助而不是开界面：在终端敲 apicase 弹出一个窗口是一种惊吓，
    // 且 git / docker / cargo 都是这个规矩
    arg_required_else_help = true,
    // 帮助排版与分组标题一并中文化。clap 的内置英文只剩 `[default: …]`
    // 这类方括号注解——那些是取值本身的注解，混排里反而是最不碍事的部分。
    help_template = "\
{about-with-newline}
用法：{usage}

{all-args}{after-help}",
    subcommand_help_heading = "命令",
    subcommand_value_name = "命令",
    next_help_heading = "选项",
    disable_help_flag = true,
    disable_version_flag = true,
    // 不要内置的 `help` 子命令：它那行说明是 clap 在 build 期插进去的，derive 管不到，
    // 一整屏中文里夹一行英文很扎眼。`apicase run --help` 完全等价，也更常用
    // （ripgrep、fd 等现代 CLI 同样只留 --help）。
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub command: Option<Command>,

    // 自己定义只为了把 clap 内置的英文说明换成中文；
    // 放在最后是因为帮助按定义序排，而 -h / -V 按惯例垫底。
    /// 显示帮助
    #[arg(short, long, global = true, action = clap::ArgAction::Help)]
    pub help: Option<bool>,

    /// 显示版本
    #[arg(short = 'V', long, action = clap::ArgAction::Version)]
    pub version: Option<bool>,
}

/// 各子命令通用的选项。`global = true` 让它们在子命令前后都能写
/// （`apicase -w /x run` 与 `apicase run -w /x` 等价）。
#[derive(Args, Debug, Clone, Default)]
pub struct GlobalOpts {
    /// 工作空间根目录（默认从目标或当前目录向上找 application.yml）
    #[arg(short = 'w', long, global = true, value_name = "目录")]
    pub workspace: Option<PathBuf>,

    /// 环境名（默认用 application.yml 里的第一套）
    #[arg(short = 'e', long, global = true, value_name = "名称")]
    pub env: Option<String>,

    /// 输出格式（默认：终端下 text，管道下 json）
    #[arg(long, global = true, value_name = "格式", value_enum)]
    pub format: Option<Format>,

    /// 等价于 --format json
    #[arg(long, global = true, conflicts_with = "format")]
    pub json: bool,

    /// 何时着色
    #[arg(long, global = true, value_name = "时机", value_enum, default_value_t = ColorWhen::Auto)]
    pub color: ColorWhen,

    /// 只输出结果，不输出进度
    #[arg(short, long, global = true)]
    pub quiet: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorWhen {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 运行用例或目录
    Run(RunArgs),

    /// 列出用例
    #[command(alias = "list")]
    Ls(LsArgs),

    /// 校验用例（只解析，不发请求）
    #[command(alias = "validate")]
    Check(CheckArgs),

    /// 查看一个用例的详情
    Show(ShowArgs),

    /// 环境与变量
    Env(EnvArgs),

    /// Cookie jar
    Cookie(CookieArgs),

    /// 运行报告
    Report(ReportArgs),

    /// 新建用例
    New(NewArgs),

    /// 把目录准备好：工作空间配置 + 命令行工具 + AGENTS.md
    Init(InitArgs),

    /// 管理 apicase 命令本身（装进 PATH / 移除 / 查状态）
    #[command(subcommand)]
    #[command(name = "self")]
    Zelf(SelfCommand),

    /// 查看用例 YAML 的格式规范
    Docs(DocsArgs),

    /// 以 MCP 服务器运行（stdio），供 AI Agent 调用
    Mcp(McpArgs),

    /// 生成 shell 补全脚本
    Completion(CompletionArgs),
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// 用例文件、目录，或 `-`（从标准输入读一份用例）。省略 = 整个工作空间
    #[arg(value_name = "目标")]
    pub targets: Vec<String>,

    /// 只跑指定请求（自动带上它的上游依赖），可重复
    #[arg(long = "step", value_name = "ID")]
    pub steps: Vec<String>,

    /// 用例之间的并发数
    #[arg(short = 'j', long, value_name = "N", default_value_t = 1)]
    pub concurrency: u32,

    /// 首个失败即停
    #[arg(long)]
    pub bail: bool,

    /// 断言失败不阻断下游请求（覆盖工作空间设置）
    #[arg(long)]
    pub continue_on_assertion_failure: bool,

    /// 追加或覆盖环境变量，形如 name=value，可重复
    #[arg(long = "var", value_name = "K=V")]
    pub vars: Vec<String>,

    /// 请求超时上限（毫秒），覆盖工作空间设置；0 = 不限制
    #[arg(long, value_name = "毫秒")]
    pub timeout: Option<u64>,

    /// 跳过 TLS 证书校验（降安全，仅用于自签名环境）
    #[arg(long)]
    pub insecure: bool,

    /// 目录不递归
    #[arg(long)]
    pub no_recursive: bool,

    /// HTML 报告落盘路径（默认 <工作空间>/.apicase/reports/<时间戳>-<目标>.html）
    #[arg(long, value_name = "文件", conflicts_with = "no_report")]
    pub report: Option<PathBuf>,

    /// 不落盘 HTML 报告
    #[arg(long)]
    pub no_report: bool,

    /// JSON 输出的详略
    #[arg(long, value_name = "级别", value_enum, default_value_t = Detail::Summary)]
    pub detail: Detail,
}

/// JSON 输出给多少内容。
///
/// **默认 summary 是刻意的**：一份 500 用例的完整报告有几 MB，无论是灌进 AI 的上下文
/// 还是管道给 jq，默认给全份都是错的默认值。要全份的场合（归档、二次分析）显式说。
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// 统计 + 失败现场
    Summary,
    /// 完整 RunReport
    Full,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    /// 目录或文件。省略 = 整个工作空间
    #[arg(value_name = "路径")]
    pub targets: Vec<PathBuf>,

    /// 按文件路径 / 用例名 / 请求 URL 过滤（子串，大小写不敏感）
    ///
    /// 只有长形式：`-q` 被全局的 `--quiet` 占着，而那个更常用。
    #[arg(long, value_name = "关键词")]
    pub query: Option<String>,

    /// 目录不递归
    #[arg(long)]
    pub no_recursive: bool,
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    /// 用例文件、目录，或 `-`（从标准输入读）。省略 = 整个工作空间
    #[arg(value_name = "目标")]
    pub targets: Vec<String>,

    /// 目录不递归
    #[arg(long)]
    pub no_recursive: bool,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    /// 用例文件
    #[arg(value_name = "用例")]
    pub target: PathBuf,

    /// 输出形态：yaml = 规范化后的原文，json = 结构化模型
    #[arg(long, value_name = "形态", value_enum, default_value_t = ShowAs::Yaml)]
    pub r#as: ShowAs,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowAs {
    Yaml,
    Json,
}

#[derive(Args, Debug)]
pub struct EnvArgs {
    #[command(subcommand)]
    pub command: Option<EnvCommand>,
}

#[derive(Subcommand, Debug)]
pub enum EnvCommand {
    /// 列出所有环境
    Ls,
    /// 显示一套环境的变量（省略名称 = 缺省环境）
    Show {
        #[arg(value_name = "名称")]
        name: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct CookieArgs {
    #[command(subcommand)]
    pub command: Option<CookieCommand>,
}

#[derive(Subcommand, Debug)]
pub enum CookieCommand {
    /// 列出 jar 里的 cookie
    Ls {
        /// 只看这个域
        #[arg(value_name = "域")]
        domain: Option<String>,
    },
    /// 删除一条 cookie
    Rm {
        #[arg(value_name = "域")]
        domain: String,
        #[arg(value_name = "路径")]
        path: String,
        #[arg(value_name = "名称")]
        name: String,
    },
    /// 清空（给了域就只清该域）
    Clear {
        #[arg(value_name = "域")]
        domain: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct ReportArgs {
    #[command(subcommand)]
    pub command: Option<ReportCommand>,
}

#[derive(Subcommand, Debug)]
pub enum ReportCommand {
    /// 列出历史报告（新的在前）
    Ls,
    /// 读一份报告的结果（省略文件 = 最近一份）
    Show {
        #[arg(value_name = "文件")]
        file: Option<PathBuf>,
        /// 只看这些状态的用例
        #[arg(long, value_name = "状态", value_enum, default_value_t = ReportFilter::All)]
        filter: ReportFilter,
        #[arg(long, value_name = "级别", value_enum, default_value_t = Detail::Summary)]
        detail: Detail,
    },
    /// 用系统默认程序打开（省略文件 = 最近一份）
    Open {
        #[arg(value_name = "文件")]
        file: Option<PathBuf>,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFilter {
    All,
    Failed,
    Error,
    /// 失败与错误（排查时最常要的那一档）
    Bad,
}

#[derive(Args, Debug)]
pub struct NewArgs {
    /// 新用例的路径（`.yml` 后缀可省略）
    #[arg(value_name = "路径")]
    pub path: PathBuf,

    /// 用例名（默认取文件名）
    #[arg(long, value_name = "名称")]
    pub name: Option<String>,

    #[arg(short = 'X', long, value_name = "方法", default_value = "GET")]
    pub method: String,

    #[arg(long, value_name = "URL", default_value = "https://example.com/api")]
    pub url: String,

    /// 已存在时覆盖
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct InitArgs {
    /// 目录（默认当前目录）
    #[arg(value_name = "目录")]
    pub dir: Option<PathBuf>,

    /// 不生成 AGENTS.md
    #[arg(long)]
    pub no_agents: bool,

    /// 不把 apicase 装进 PATH
    #[arg(long)]
    pub no_link: bool,
}

/// `self` 是 Rust 关键字，枚举变体只能叫别的；命令名由 `#[command(name = "self")]` 指定。
#[derive(Subcommand, Debug)]
pub enum SelfCommand {
    /// 查看 apicase 命令在不在 PATH 里、指向哪
    Status,
    /// 把 apicase 装进 PATH（软链，不覆盖别人的同名命令）
    Install,
    /// 从 PATH 移除
    Uninstall,
}

#[derive(Args, Debug)]
pub struct DocsArgs {
    /// 主题；省略 = case。用 `apicase docs --topics` 看有哪些
    #[arg(value_name = "主题")]
    pub topic: Option<String>,

    /// 列出所有主题
    #[arg(long)]
    pub topics: bool,
}

#[derive(Args, Debug)]
pub struct McpArgs {
    /// 无额外选项：工作空间用全局的 -w，传输固定为 stdio
    #[arg(skip)]
    pub _reserved: (),
}

#[derive(Args, Debug)]
pub struct CompletionArgs {
    #[arg(value_name = "SHELL", value_enum)]
    pub shell: clap_complete::Shell,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// clap 的定义有不少运行期才报的约束（重名的短选项、坏的 conflicts_with 目标）。
    /// 这条断言把它们提前到 `cargo test`。
    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_options_work_before_and_after_the_subcommand() {
        for argv in [
            vec!["apicase", "-w", "/x", "run", "a.yml"],
            vec!["apicase", "run", "-w", "/x", "a.yml"],
        ] {
            let cli = Cli::try_parse_from(&argv).expect("应能解析");
            assert_eq!(cli.global.workspace.as_deref(), Some(std::path::Path::new("/x")), "{argv:?}");
            match cli.command {
                Some(Command::Run(a)) => assert_eq!(a.targets, vec!["a.yml"]),
                other => panic!("应是 run：{other:?}"),
            }
        }
    }

    #[test]
    fn run_defaults_are_conservative() {
        let cli = Cli::try_parse_from(["apicase", "run"]).expect("应能解析");
        let Some(Command::Run(a)) = cli.command else { panic!("应是 run") };
        assert_eq!(a.concurrency, 1, "默认串行");
        assert!(!a.bail);
        assert!(!a.insecure);
        assert!(a.targets.is_empty(), "省略目标 = 整个工作空间");
        assert_eq!(a.detail, Detail::Summary, "JSON 默认给摘要，全份要显式说");
    }

    /// --report 与 --no-report 互斥：同时给了意思自相矛盾，该在解析期就拦下
    #[test]
    fn report_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["apicase", "run", "--report", "a.html", "--no-report"]).is_err());
        assert!(Cli::try_parse_from(["apicase", "run", "--json", "--format", "text"]).is_err());
    }

    #[test]
    fn aliases_resolve() {
        for (argv, want) in [(["apicase", "list"], "ls"), (["apicase", "validate"], "check")] {
            let cli = Cli::try_parse_from(argv).expect("别名应能解析");
            let got = match cli.command {
                Some(Command::Ls(_)) => "ls",
                Some(Command::Check(_)) => "check",
                other => panic!("意外的子命令：{other:?}"),
            };
            assert_eq!(got, want);
        }
    }
}
