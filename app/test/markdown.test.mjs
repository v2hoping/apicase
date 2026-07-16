// markdown.tsx 单元测试：块级 + 行内渲染、转义、URL 安全、占位符不误伤正文。
import { loadModule, eq, ok, has, hasnt, report } from "./harness.mjs";

const { renderMarkdown } = await loadModule("src/markdown.tsx");

// ── 行内 ──
has(renderMarkdown("**粗**"), "<strong>粗</strong>", "加粗（星号）");
has(renderMarkdown("__粗__"), "<strong>粗</strong>", "加粗（下划线，词边界）");
has(renderMarkdown("*斜*"), "<em>斜</em>", "斜体");
has(renderMarkdown("~~删~~"), "<del>删</del>", "删除线");
has(renderMarkdown("`code`"), "<code>code</code>", "行内代码");
has(renderMarkdown("[示例](https://a.com)"), '<a href="https://a.com"', "链接 href");
has(renderMarkdown("[示例](https://a.com)"), 'target="_blank"', "链接新窗口");
has(renderMarkdown("![图](https://a.com/x.png)"), '<img class="md-img" alt="图" src="https://a.com/x.png"', "图片");

// ── HTML 转义（防注入）──
has(renderMarkdown("危险 <script>alert(1)</script>"), "&lt;script&gt;", "尖括号转义");
hasnt(renderMarkdown("危险 <script>alert(1)</script>"), "<script>", "不产生真实 script 标签");
has(renderMarkdown("a & b < c"), "&amp;", "& 转义");

// ── URL 安全：javascript:/data: 拦截为 # ──
has(renderMarkdown("[x](javascript:alert(1))"), 'href="#"', "javascript: URL 拦截");
has(renderMarkdown("[x](JavaScript:alert(1))"), 'href="#"', "大小写混淆的 javascript: 拦截");

// ── 关键回归：行内代码占位符不误伤正文中的「空格+数字+空格」──
const r = renderMarkdown("共 3 步，见 `code` 与 5 个 **粗** 字");
has(r, "共 3 步", "正文中「3」保留");
has(r, "与 5 个", "正文中「5」保留");
has(r, "<code>code</code>", "同段落的行内代码正常");
hasnt(r, "undefined", "无 undefined 泄漏");

// ── 行内代码内的 * 不被斜体化 ──
has(renderMarkdown("`a*b*c`"), "<code>a*b*c</code>", "代码内星号不解析为斜体");

// ── 标题 ──
has(renderMarkdown("# 一级"), "<h1", "h1");
has(renderMarkdown("### 三级"), "<h3", "h3");
hasnt(renderMarkdown("#无空格"), "<h1", "缺空格不算标题");

// ── 段落 ──
has(renderMarkdown("第一行\n第二行"), "<br />", "段内换行→<br>");
ok((renderMarkdown("段一\n\n段二").match(/<p /g) || []).length === 2, "空行分隔为两段");

// ── 围栏代码块 ──
const fenced = renderMarkdown("```js\nconst a = 1 < 2;\n```");
has(fenced, "<pre", "代码块 pre");
has(fenced, "language-js", "代码块语言 class");
has(fenced, "1 &lt; 2", "代码块内容转义");
hasnt(fenced, "<code>const", "代码块不产生行内 code");

// ── 列表 ──
const ul = renderMarkdown("- a\n- b\n- c");
has(ul, "<ul", "无序列表");
ok((ul.match(/<li>/g) || []).length === 3, "三个列表项");
const ol = renderMarkdown("1. 一\n2. 二");
has(ol, "<ol", "有序列表");
// 嵌套
const nested = renderMarkdown("- 顶层\n  - 子项");
has(nested, "<ul", "嵌套列表外层");
ok((nested.match(/<ul/g) || []).length === 2, "嵌套产生两层 ul");

// ── 引用 ──
has(renderMarkdown("> 引用文本"), "<blockquote", "引用块");

// ── 分隔线 ──
has(renderMarkdown("---"), "<hr", "分隔线");

// ── 表格 ──
const table = renderMarkdown("| A | B |\n| --- | --- |\n| 1 | 2 |");
has(table, "<table", "表格");
has(table, "<th>A</th>", "表头单元格");
has(table, "<td>1</td>", "表体单元格");
// 转义竖线 \| 不应切分单元格
const tesc = renderMarkdown("| 表达式 | 结果 |\n| --- | --- |\n| a \\| b | 或 |");
has(tesc, "<td>a | b</td>", "转义竖线还原为字面 | 且不切分");

// ── 空输入 ──
eq(renderMarkdown(""), "", "空串→空");
eq(renderMarkdown("   \n  "), "", "纯空白→空");

// ══════ 代码审查修复的回归用例 ══════

// #1 死循环：非常规围栏开启行必须能正常返回并渲染为 <pre>（返回即证明未死循环）
for (const f of ["```c++", "```objective-c", '```js title="x"', "````"]) {
  const r = renderMarkdown(f + "\ncode line\n```");
  has(r, "<pre", `围栏 ${JSON.stringify(f)} 正常渲染（不死循环）`);
}
has(renderMarkdown("```c++\nint a;\n```"), 'language-c++', "c++ 语言 class 提取");

// #4 强调不得破坏链接/图片 URL 中的 * 与 __
const linkStar = renderMarkdown("[console](https://ci.example.com/job/*/console)");
has(linkStar, "https://ci.example.com/job/*/console", "链接 URL 中的 * 原样保留");
hasnt(linkStar, "<em>", "链接 URL 的 * 不产生 <em>");
const linkDunder = renderMarkdown("[init](https://x/__init__.py)");
has(linkDunder, "https://x/__init__.py", "链接 URL 中的 __ 原样保留");
hasnt(linkDunder, "<strong>", "链接 URL 的 __ 不产生 <strong>");

// #5 正文中的孤立星号不得被斜体化（flanking）
const prose = renderMarkdown("面积 = w * h * scale");
has(prose, "w * h * scale", "正文 * 原样保留");
hasnt(prose, "<em>", "正文 * 不产生 <em>");
hasnt(renderMarkdown("匹配 *.js 与 *.ts 文件"), "<em>", "glob 星号不产生 <em>");
has(renderMarkdown("变量 my_var 和 a_b_c"), "my_var", "snake_case 不被下划线斜体/加粗破坏");

// #6 紧邻文本行（无空行）的表格应被识别
const tblTight = renderMarkdown("字段：\n| a | b |\n|---|---|\n| 1 | 2 |");
has(tblTight, "<table", "紧邻文本的表格被识别");
has(tblTight, "<th>a</th>", "紧邻表格表头");

// #7 单列表格
const tbl1 = renderMarkdown("| A |\n| --- |\n| 1 |");
has(tbl1, "<table", "单列表格被识别");
has(tbl1, "<th>A</th>", "单列表头");
has(tbl1, "<td>1</td>", "单列表体");
// hr 的 --- 不应被误判为表格分隔
has(renderMarkdown("---"), "<hr", "裸 --- 仍是 hr 而非表格");

// #8 title 含实体不应导致整体不匹配
const titleAmp = renderMarkdown('[t](http://x "a & b")');
has(titleAmp, '<a href="http://x"', "含 & 的 title 链接仍成链接");
has(titleAmp, 'title="a &amp; b"', "title 内容转义保留");

// #9 代码占位不得进入 URL（![a](`c`) 不产生 src="<code>"）
const imgCode = renderMarkdown("![a](`c`)");
hasnt(imgCode, 'src="<code>', "代码占位不落入 img src");
// 嵌套：链接文本内的代码正常
has(renderMarkdown("[`c`](http://x)"), '<a href="http://x"', "链接文本含代码：链接成立");
has(renderMarkdown("[`c`](http://x)"), "<code>c</code>", "链接文本含代码：代码渲染");

// #10 data:image 图片放行，data: 链接仍拦截
has(renderMarkdown("![d](data:image/png;base64,AAAA)"), 'src="data:image/png;base64,AAAA"', "data:image 图片放行");
has(renderMarkdown("[x](data:text/html,evil)"), 'href="#"', "data: 链接仍拦截");

report();
