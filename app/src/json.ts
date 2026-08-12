/* 响应体的 JSON 美化与语法着色（纯逻辑，渲染成节点的部分在 App.tsx 的 renderBody）。

   为什么手写而不引高亮库：这里只需认四类 token（键 / 字符串 / 字面量 / 标点），
   而任何一个通用高亮库都会为覆盖 N 种语言把整套词法体系搬进来。同本项目的
   markdown 渲染器（markdown.tsx）一样的取法——够用的最小实现，不引依赖。 */

export type JsonTokenClass = "" | "json-key" | "json-str" | "json-num" | "json-punct";

export interface JsonToken {
  text: string;
  cls: JsonTokenClass;
}

/** 超过这个长度只美化不着色：建节点的开销随体积线性涨，而几 MB 的响应
    本就是拿来滚动扫的、不是逐字读的，为它卡住界面不值当。 */
export const JSON_COLOR_LIMIT = 200_000;

/** 美化 JSON；不是合法 JSON 时返回 null（调用方原样显示，同旧行为）。 */
export function prettyJson(body: string): string | null {
  try {
    return JSON.stringify(JSON.parse(body), null, 2);
  } catch {
    return null;
  }
}

/* 字符串分支写在最前：它会把整个带引号的串整体吃掉，
   串里的数字（如 "user123"、时间戳字符串）便不会被数字分支误标成数值。 */
const TOKEN = /("(?:\\.|[^"\\])*")(\s*:)?|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null)/g;

/** 把美化后的 JSON 切成带类名的 token；未匹配的部分（缩进、括号、逗号）落到 cls: ""。 */
export function tokenizeJson(pretty: string): JsonToken[] {
  const out: JsonToken[] = [];
  let last = 0;
  for (const m of pretty.matchAll(TOKEN)) {
    const i = m.index;
    if (i > last) out.push({ text: pretty.slice(last, i), cls: "" });
    if (m[1] !== undefined) {
      // 字符串后面紧跟冒号即为键，否则是字符串值
      out.push({ text: m[1], cls: m[2] ? "json-key" : "json-str" });
      if (m[2]) out.push({ text: m[2], cls: "json-punct" });
    } else {
      out.push({ text: m[3], cls: "json-num" });
    }
    last = i + m[0].length;
  }
  if (last < pretty.length) out.push({ text: pretty.slice(last), cls: "" });
  return out;
}
