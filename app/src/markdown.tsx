// 轻量 markdown 渲染 + 编辑器（零依赖，与项目「自研解析」风格一致）。
// 供两处复用：① Request 编辑器「文档」标签；② 文件树中 .md 文件的编辑/预览。
// 渲染前对文本做 HTML 转义，避免注入；仅生成受控标签。
import { useState } from "react";

// ── HTML 转义 ─────────────────────────────────────
function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}
// URL 仅允许安全协议；图片额外放行 data:image/*（`<img>` 中的 data 图不会执行脚本）
function safeUrl(u: string, allowDataImage = false): string {
  const t = u.trim();
  if (allowDataImage && /^data:image\//i.test(t)) return t;
  if (/^(javascript|data|vbscript):/i.test(t)) return "#";
  return t;
}

// ── 行内解析（在已转义文本上做）──────────────────────
// 顺序：代码 → 图片 → 链接 均先抽成 NUL 占位（保护其内容/URL 不被后续加粗/斜体改动），
// 再跑加粗/斜体/删除线（带 flanking 规则，避免误伤正文星号与 URL），最后迭代还原占位（允许嵌套）。
function renderInline(raw: string): string {
  let s = escapeHtml(raw);
  const tokens: string[] = [];
  const stash = (html: string) => {
    tokens.push(html);
    return `\u0000${tokens.length - 1}\u0000`;
  };
  // 行内代码
  s = s.replace(/`([^`]+)`/g, (_m, c) => stash(`<code>${c}</code>`));
  // 图片 ![alt](src "title")——URL 不含空白/)/NUL；title 允许含实体
  s = s.replace(/!\[([^\]]*)\]\(([^)\s\u0000]+)(?:\s+&quot;([\s\S]*?)&quot;)?\)/g, (_m, alt, src, title) => {
    const t = title ? ` title="${title}"` : "";
    return stash(`<img class="md-img" alt="${alt}" src="${safeUrl(src, true)}"${t} />`);
  });
  // 链接 [text](href "title")
  s = s.replace(/\[([^\]]+)\]\(([^)\s\u0000]+)(?:\s+&quot;([\s\S]*?)&quot;)?\)/g, (_m, text, href, title) => {
    const t = title ? ` title="${title}"` : "";
    return stash(`<a href="${safeUrl(href)}" target="_blank" rel="noreferrer noopener"${t}>${text}</a>`);
  });
  // 加粗（先于斜体）、斜体、删除线——flanking：定界符内侧不得为空白，避免 "w * h" 与 URL 星号被误配
  s = s.replace(/\*\*(?!\s)([^\n]+?)(?<!\s)\*\*/g, "<strong>$1</strong>");
  s = s.replace(/(?<![A-Za-z0-9])__(?!\s)([^\n]+?)(?<!\s)__(?![A-Za-z0-9])/g, "<strong>$1</strong>");
  s = s.replace(/\*(?!\s)([^*\n]+?)(?<!\s)\*/g, "<em>$1</em>");
  s = s.replace(/~~(?!\s)([^~\n]+?)(?<!\s)~~/g, "<del>$1</del>");
  // 迭代还原占位（链接文本内可能嵌套代码占位）
  let guard = 0;
  while (s.includes("\u0000") && guard++ < 20) {
    s = s.replace(/\u0000(\d+)\u0000/g, (_m, i) => tokens[Number(i)] ?? "");
  }
  return s;
}

// ── 表格（GFM 管道表）─────────────────────────────
function splitRow(line: string): string[] {
  let l = line.trim();
  if (l.startsWith("|")) l = l.slice(1);
  if (l.endsWith("|")) l = l.slice(0, -1);
  // 仅按未转义的 | 分割，再把 \| 还原为字面 |
  return l.split(/(?<!\\)\|/).map((c) => c.replace(/\\\|/g, "|").trim());
}
// 分隔行：必须含 | （规避与 hr 的 --- 混淆），且每格均为 :?-+:? —— 兼容单列表格
function isTableSep(line: string): boolean {
  if (!line.includes("|")) return false;
  const cells = splitRow(line);
  return cells.length >= 1 && cells.every((c) => /^:?-{1,}:?$/.test(c));
}
// 表格起始：当前行含 | 且下一行是分隔行
function isTableStart(lines: string[], i: number): boolean {
  return lines[i].includes("|") && i + 1 < lines.length && isTableSep(lines[i + 1]);
}

// ── 块级解析 ───────────────────────────────────────
function renderBlocks(src: string): string {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  let i = 0;
  const isBlank = (l: string) => l.trim() === "";

  while (i < lines.length) {
    const line = lines[i];

    // 空行
    if (isBlank(line)) {
      i++;
      continue;
    }

    // 围栏代码块 ```lang（宽松：允许 3+ 反引号与任意 info string，避免与 isBlockStart 不一致而死循环）
    const fence = line.match(/^\s*`{3,}(.*)$/);
    if (fence) {
      const lang = (fence[1].trim().match(/^[\w+#.-]+/) || [""])[0];
      const buf: string[] = [];
      i++;
      while (i < lines.length && !/^\s*`{3,}\s*$/.test(lines[i])) {
        buf.push(lines[i]);
        i++;
      }
      i++; // 跳过结尾围栏
      const cls = lang ? ` class="language-${lang}"` : "";
      out.push(`<pre class="md-pre"><code${cls}>${escapeHtml(buf.join("\n"))}</code></pre>`);
      continue;
    }

    // 标题 # ~ ######
    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) {
      const level = h[1].length;
      out.push(`<h${level} class="md-h md-h${level}">${renderInline(h[2].trim())}</h${level}>`);
      i++;
      continue;
    }

    // 分隔线
    if (/^\s*(-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      out.push(`<hr class="md-hr" />`);
      i++;
      continue;
    }

    // 引用块（连续 > ）
    if (/^\s*>/.test(line)) {
      const buf: string[] = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) {
        buf.push(lines[i].replace(/^\s*>\s?/, ""));
        i++;
      }
      out.push(`<blockquote class="md-quote">${renderBlocks(buf.join("\n"))}</blockquote>`);
      continue;
    }

    // 表格（当前行含 | 且下一行是分隔行）
    if (isTableStart(lines, i)) {
      const header = splitRow(line);
      i += 2; // 跳过表头与分隔行
      const rows: string[][] = [];
      while (i < lines.length && lines[i].includes("|") && !isBlank(lines[i])) {
        rows.push(splitRow(lines[i]));
        i++;
      }
      const thead = `<tr>${header.map((c) => `<th>${renderInline(c)}</th>`).join("")}</tr>`;
      const tbody = rows
        .map((r) => `<tr>${header.map((_, ci) => `<td>${renderInline(r[ci] ?? "")}</td>`).join("")}</tr>`)
        .join("");
      out.push(`<table class="md-table"><thead>${thead}</thead><tbody>${tbody}</tbody></table>`);
      continue;
    }

    // 列表（有序 / 无序，支持按缩进的一层嵌套）
    if (/^\s*([-*+]|\d+[.)])\s+/.test(line)) {
      const [html, next] = renderList(lines, i);
      out.push(html);
      i = next;
      continue;
    }

    // 段落：聚合连续普通行，直到空行 / 块级起始 / 表格起始
    const para: string[] = [];
    while (i < lines.length && !isBlank(lines[i]) && !isBlockStart(lines[i]) && !isTableStart(lines, i)) {
      para.push(lines[i]);
      i++;
    }
    if (para.length === 0) {
      // 安全兜底：该行被判为块级起始却无分支消费它 —— 作为普通文本输出并前进，杜绝 i 不前进的死循环
      out.push(`<p class="md-p">${renderInline(line.trim())}</p>`);
      i++;
      continue;
    }
    out.push(`<p class="md-p">${para.map((l) => renderInline(l.trim())).join("<br />")}</p>`);
  }

  return out.join("\n");
}

// 是否为块级结构的起始行（用于段落聚合时提前中断）；须与 renderBlocks 各消费分支一致，否则会死循环
function isBlockStart(line: string): boolean {
  return (
    /^\s*`{3,}/.test(line) ||
    /^(#{1,6})\s+/.test(line) ||
    /^\s*(-{3,}|\*{3,}|_{3,})\s*$/.test(line) ||
    /^\s*>/.test(line) ||
    /^\s*([-*+]|\d+[.)])\s+/.test(line)
  );
}

// 列表构建：从 start 行起，按缩进分组，返回 [html, 下一行索引]
function renderList(lines: string[], start: number): [string, number] {
  const baseIndent = lines[start].match(/^(\s*)/)![1].length;
  const ordered = /^\s*\d+[.)]\s+/.test(lines[start]);
  const items: string[] = [];
  let i = start;

  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === "") {
      // 允许列表项间的单个空行
      if (i + 1 < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i + 1])) {
        i++;
        continue;
      }
      break;
    }
    const m = line.match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/);
    if (!m) break;
    const indent = m[1].length;
    if (indent < baseIndent) break;
    if (indent > baseIndent) {
      // 更深缩进 → 作为上一项的嵌套子列表
      const [sub, next] = renderList(lines, i);
      if (items.length) items[items.length - 1] += sub;
      else items.push(sub);
      i = next;
      continue;
    }
    items.push(renderInline(m[3].trim()));
    i++;
  }

  const tag = ordered ? "ol" : "ul";
  const body = items.map((it) => `<li>${it}</li>`).join("");
  return [`<${tag} class="md-list">${body}</${tag}>`, i];
}

/** 渲染 markdown 文本为 HTML 字符串（已转义，安全用于 dangerouslySetInnerHTML）。 */
export function renderMarkdown(md: string): string {
  if (!md || !md.trim()) return "";
  return renderBlocks(md);
}

/** 只读渲染视图。 */
export function Markdown({ text, className = "" }: { text: string; className?: string }) {
  const html = renderMarkdown(text);
  if (!html) return <div className={`markdown-view is-empty ${className}`}>（空文档）</div>;
  return <div className={`markdown-view ${className}`} dangerouslySetInnerHTML={{ __html: html }} />;
}

/**
 * Markdown 编辑器：编辑 / 预览 / 分屏三种模式。
 * 完全受控——value + onChange。compact 用于嵌入较小面板（如请求「文档」标签）。
 */
export function MarkdownEditor({
  value,
  onChange,
  compact = false,
  placeholder = "在此编写 Markdown 文档…",
}: {
  value: string;
  onChange: (v: string) => void;
  compact?: boolean;
  placeholder?: string;
}) {
  const [mode, setMode] = useState<"edit" | "preview" | "split">(compact ? "edit" : "split");
  return (
    <div className={`md-editor ${compact ? "compact" : ""} mode-${mode}`}>
      <div className="md-toolbar">
        <div className="md-modes">
          <button type="button" className={`md-mode-btn ${mode === "edit" ? "active" : ""}`} onClick={() => setMode("edit")}>
            编辑
          </button>
          <button type="button" className={`md-mode-btn ${mode === "preview" ? "active" : ""}`} onClick={() => setMode("preview")}>
            预览
          </button>
          <button type="button" className={`md-mode-btn ${mode === "split" ? "active" : ""}`} onClick={() => setMode("split")}>
            分屏
          </button>
        </div>
      </div>
      <div className="md-body">
        {mode !== "preview" && (
          <textarea
            className="md-input"
            value={value}
            spellCheck={false}
            placeholder={placeholder}
            onChange={(e) => onChange(e.target.value)}
          />
        )}
        {mode !== "edit" && (
          <div className="md-preview">
            <Markdown text={value} />
          </div>
        )}
      </div>
    </div>
  );
}
