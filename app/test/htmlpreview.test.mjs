// 任意 HTML 预览的安全注入：预览一份来源不明的本地文件，不该产生任何对外流量。
//
// sandbox 属性挡的是脚本与同源能力，但 `<img src="https://…">` 这类资源引用**不需要脚本**
// 就会发出请求（谁打开过这份文件、什么时候打开，对方一清二楚）。CSP 才是挡这个的那道。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { withStrictCsp } = await loadModule("src/HtmlPreview.tsx");

const CSP = 'http-equiv="Content-Security-Policy"';

// ── 注入位置 ──────────────────────────────────────

{
  const out = withStrictCsp("<!doctype html><html><head><title>x</title></head><body>hi</body></html>");
  ok(out.includes(CSP), "注入了 CSP");
  ok(out.indexOf(CSP) < out.indexOf("<title>"), "落在 head 里且排在其它标签之前");
  ok(out.includes("<body>hi</body>"), "原文其余部分不动");
}
{
  // 片段式 HTML（没有 html/head 标签）：浏览器会自行补全结构，meta 落在最前面同样生效
  const out = withStrictCsp("<p>只有一段</p>");
  ok(out.startsWith("<meta"), "无 head 时插到最前面");
  ok(out.includes("<p>只有一段</p>"), "原文保留");
}
{
  const out = withStrictCsp('<html><head lang="zh"><meta charset="utf-8"></head><body></body></html>');
  ok(out.indexOf(CSP) < out.indexOf("charset"), "带属性的 head 标签也能识别");
}
{
  const out = withStrictCsp("<HTML><HEAD></HEAD></HTML>");
  ok(out.indexOf(CSP) < out.indexOf("</HEAD>"), "标签大小写不敏感");
}

// ── 策略内容：默认全禁，只放行渲染静态内容必需的两样 ──

{
  const csp = /content="([^"]+)"/.exec(withStrictCsp("<p>x</p>"))[1];
  ok(csp.includes("default-src 'none'"), "默认什么都不许加载");
  ok(csp.includes("style-src 'unsafe-inline'"), "内联样式放行（否则页面全是裸文本）");
  ok(csp.includes("img-src data:"), "内嵌图片放行");
  ok(!/img-src[^;]*https?:/.test(csp), "外部图片不放行——这正是被动追踪的入口");
  ok(!csp.includes("script-src"), "不单独放行脚本：default-src 'none' 已经挡住");
  ok(!csp.includes("connect-src http"), "不放行任何对外连接");
}

// 空文档不该抛异常（读到一个空 .html 文件是完全可能的）
eq(typeof withStrictCsp(""), "string", "空输入照常返回字符串");

report();
