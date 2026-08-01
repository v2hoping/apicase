// 任意 HTML 文件的可视化预览。
//
// 与运行报告的 iframe 不同，这里渲染的是**来源不明的页面**——工作空间里的 .html 可能来自
// 任何地方。所以默认按"只看不跑"处理：
//
// - `sandbox=""`：不给任何能力（脚本、表单、弹窗、same-origin 全禁）；
// - 注入 CSP `default-src 'none'`：即便没有脚本，`<img src="https://…">` 这类外部资源
//   一样会发出请求（谁打开过这份文件、什么时候打开的，对方看得一清二楚）。
//   预览一份本地文件不该产生任何对外流量。
//
// 需要交互时由用户**显式**点「允许运行脚本」——那一下是授权，不能替他做主。
import { useState } from "react";

/** 只看不跑模式下注入的策略：允许内联样式与内嵌图片，其余一律不放行。 */
const STRICT_CSP =
  "default-src 'none'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:";

/**
 * 把 CSP 作为第一个 `<meta>` 注入文档头。
 *
 * 插在 `<head>` 之后；没有 `<head>` 就插在最前面——片段式 HTML（没有 html/head 标签）
 * 浏览器会自行补全结构，`<meta>` 落在最前面同样生效。
 */
export function withStrictCsp(html: string): string {
  const meta = `<meta http-equiv="Content-Security-Policy" content="${STRICT_CSP}">`;
  const head = /<head[^>]*>/i.exec(html);
  if (head) {
    const at = head.index + head[0].length;
    return html.slice(0, at) + meta + html.slice(at);
  }
  return meta + html;
}

export function HtmlPreview({ html }: { html: string }) {
  // 每次切换重新挂载 iframe：sandbox 属性变了必须重新加载文档才生效
  const [allowScripts, setAllowScripts] = useState(false);
  return (
    <div className="html-preview">
      <div className="html-preview-bar">
        {allowScripts ? (
          <>
            <span className="html-preview-note warn">⚠ 已允许此页面运行脚本与联网。</span>
            <button className="html-preview-btn" onClick={() => setAllowScripts(false)}>
              重新阻止
            </button>
          </>
        ) : (
          <>
            <span className="html-preview-note">已阻止脚本运行与外部请求（只渲染静态内容）。</span>
            <button className="html-preview-btn" onClick={() => setAllowScripts(true)}>
              允许运行脚本
            </button>
          </>
        )}
      </div>
      <iframe
        key={allowScripts ? "live" : "static"}
        className="html-preview-frame"
        title="HTML 预览"
        // 两种模式都不给 allow-same-origin：给了就等于把 iframe 放进同源，
        // sandbox 形同虚设（脚本能直接读写宿主页面）
        sandbox={allowScripts ? "allow-scripts" : ""}
        srcDoc={allowScripts ? html : withStrictCsp(html)}
      />
    </div>
  );
}
