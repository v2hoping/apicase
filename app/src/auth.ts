// 认证的**显示元信息**：下拉里的通名、一句话说明、面板底部的报文预览。
//
// 认证的**实现**全在 Rust（`core/src/auth.rs` + `core/src/request.rs`）：
// 静态头（Basic / Bearer / API Key）的组装、Digest 的 401 challenge 摘要与重发、
// OAuth 2.0 的 token 交换与缓存去重，一件不落。前端不发请求，因此也就不需要
// 自带一份 MD5、一份 token 缓存——那些曾经存在的代码现在是 Rust 的单测在守着。
//
// 留在这里的只有"给人看"的部分。
import { AuthType } from "./case";
import { ReqDraft, utf8Base64 } from "./draft";

/** 认证方式的显示元信息（命名沿用 Postman / Insomnia / Bruno 的通行叫法）。 */
export interface AuthTypeMeta {
  value: AuthType;
  label: string; // 下拉与标题用英文通名
  zh: string; // 一句话中文说明
}

export const AUTH_TYPE_METAS: AuthTypeMeta[] = [
  { value: "none", label: "No Auth", zh: "不附加任何认证信息" },
  { value: "basic", label: "Basic Auth", zh: "用户名 + 密码，base64 编码后随请求头发送" },
  { value: "bearer", label: "Bearer Token", zh: "令牌（如 JWT）放入 Authorization 头" },
  { value: "apikey", label: "API Key", zh: "自定义键值，放在请求头或 URL 查询参数中" },
  { value: "digest", label: "Digest Auth", zh: "挑战-应答式摘要认证，密码不明文上网" },
  { value: "oauth2", label: "OAuth 2.0", zh: "客户端凭据模式：发送前自动换取 access_token" },
];

export function authLabel(t: AuthType): string {
  return AUTH_TYPE_METAS.find((m) => m.value === t)?.label ?? t;
}

/**
 * 面板底部一行预览：这种认证方式最终往报文上附加什么，所见即所得，省得用户去猜。
 *
 * Basic 的 base64 在这里现算——它是纯函数、没有副作用，为一行预览走一趟 IPC
 * 反而会让面板闪一下。Digest 与 OAuth 2.0 的值要发请求才知道，故只描述行为。
 */
export function authPreview(d: ReqDraft): { label: string; code: string } | null {
  switch (d.authType) {
    case "basic":
      return { label: "附加请求头", code: `Authorization: Basic ${utf8Base64(`${d.authBasicUser}:${d.authBasicPass}`)}` };
    case "bearer":
      return { label: "附加请求头", code: `Authorization: Bearer ${d.authBearerToken || "<token>"}` };
    case "apikey":
      return d.authApikeyIn === "header"
        ? { label: "附加请求头", code: `${d.authApikeyKey || "<键名>"}: ${d.authApikeyValue}` }
        : { label: "附加查询参数", code: `${d.authApikeyKey || "<键名>"}=${d.authApikeyValue}` };
    case "digest":
      return {
        label: "附加请求头",
        code: `Authorization: Digest username="${d.authDigestUser || "<用户名>"}", realm=…, response=…（收到 401 challenge 后计算并重发）`,
      };
    case "oauth2":
      return {
        label: "附加请求头",
        code: `Authorization: Bearer <access_token>（发送前向 ${d.authOauth2TokenUrl || "<token 端点>"} 换取）`,
      };
    default:
      return null;
  }
}
