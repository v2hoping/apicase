// 通用请求编辑器：单请求 case 与多请求 flow 的每个请求都复用它。
// 完全受控——父组件持有 ReqDraft，本组件只读 value、通过 onChange 汇报变更。
// 切换 step 时父组件用 key 强制重挂载，从而重置内部 Tab 等瞬时状态。
import { useEffect, useRef, useState, type KeyboardEvent as KeyEvent } from "react";
import { KV, FormItem, AuthType, BodyType, Assertion, AssertOp, ASSERT_OPS, RequestOutput, splitQueryFromUrl, mergeQueryIntoUrl } from "./case";
import { suggestTargets, suggestValue, type RespLite, type Suggestion } from "./assertPath";
import { open } from "@tauri-apps/plugin-dialog";
import { ReqDraft, DEFAULT_CONTENT_TYPE, guessContentType } from "./draft";
import { AUTH_TYPE_METAS, authPreview } from "./auth";
import { MarkdownEditor } from "./markdown";
import { VarInput } from "./VarInput";

export const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
// 请求体类型平铺展示（顺序、命名对齐 Apifox）
const BODY_TYPES: { value: BodyType; label: string }[] = [
  { value: "none", label: "none" },
  { value: "form-data", label: "form-data" },
  { value: "form-urlencoded", label: "x-www-form-urlencoded" },
  { value: "json", label: "JSON" },
  { value: "xml", label: "XML" },
  { value: "text", label: "Text" },
  { value: "binary", label: "Binary" },
];
const PROTOCOLS = ["http"]; // 通信协议：当前仅 http，后续可扩展 grpc 等

export function methodClass(m: string): string {
  return `method-${m.toLowerCase()}`;
}

// 行删除图标：线条描边垃圾桶（currentColor 跟随文字色，hover 变红由 .row-del 控制）
function TrashIcon() {
  return (
    <svg className="trash-ico" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <path
        fill="none"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M3 4.5h10M6.5 4.5V3.4a.9.9 0 0 1 .9-.9h1.2a.9.9 0 0 1 .9.9v1.1M11.8 4.5l-.55 8.05a1 1 0 0 1-1 .95H5.75a1 1 0 0 1-1-.95L4.2 4.5M6.7 7.1v3.9M9.3 7.1v3.9"
      />
    </svg>
  );
}

// 自定义下拉：替代原生 <select>，避免系统弹层盖住控件、带灰白阴影。
// 选项面板始终固定在控件正下方；点击外部或按 Esc 关闭。
type SelectOption = { value: string; label: string };
export function Select({
  value,
  options,
  onChange,
  className = "",
  ariaLabel,
  optionClassName,
  disabled,
  placeholder,
}: {
  value: string;
  options: SelectOption[];
  onChange: (v: string) => void;
  className?: string;
  ariaLabel?: string;
  optionClassName?: (value: string) => string; // 按选项值追加类名（如方法下拉逐项配色）
  disabled?: boolean; // 无可选项时禁用（如工作空间内没有证书文件）
  placeholder?: string; // 值为空且无匹配选项时的占位文案
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  const current = options.find((o) => o.value === value);
  const empty = !current && !value;
  return (
    <div className={`ui-select ${open ? "is-open" : ""} ${className}`} ref={ref}>
      <button
        type="button"
        className="ui-select-trigger"
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
      >
        <span className={`ui-select-value ${empty && placeholder ? "is-placeholder" : ""}`}>
          {current?.label ?? (empty ? (placeholder ?? "") : value)}
        </span>
        <svg className="ui-select-caret" viewBox="0 0 12 12" width="12" height="12" aria-hidden="true">
          <path d="M3 4.5L6 7.5L9 4.5" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>
      {open && (
        <div className="ui-select-menu">
          {options.map((o) => (
            <button
              type="button"
              key={o.value}
              className={`ui-select-option ${o.value === value ? "is-active" : ""} ${optionClassName?.(o.value) ?? ""}`}
              onClick={() => {
                onChange(o.value);
                setOpen(false);
              }}
            >
              <span className="ui-select-option-label">{o.label}</span>
              {o.value === value && <span className="ui-select-check">✓</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// 密码可见性切换图标（off = 当前隐藏，画一道斜杠）
function EyeIcon({ off }: { off?: boolean }) {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <g fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
        <path d="M1.6 8S4.1 3.8 8 3.8 14.4 8 14.4 8 11.9 12.2 8 12.2 1.6 8 1.6 8Z" />
        <circle cx="8" cy="8" r="1.9" />
        {off && <path d="M3.2 12.8 12.8 3.2" />}
      </g>
    </svg>
  );
}

// 认证里的普通文本字段：支持 ${{变量}} 高亮（令牌/用户名常来自环境变量）
function AuthText({
  label,
  value,
  onChange,
  placeholder,
  isVarSet,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  isVarSet: (name: string) => boolean;
}) {
  return (
    <div className="field-row">
      <label>{label}</label>
      <VarInput className="auth-input" wrapClassName="grow" value={value} placeholder={placeholder} onChange={onChange} isVarSet={isVarSet} />
    </div>
  );
}

// 密钥字段：默认掩码，点眼睛切明文（切换后才做变量高亮——掩码态下高亮没有意义）
function AuthSecret({
  label,
  value,
  onChange,
  placeholder,
  isVarSet,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  isVarSet: (name: string) => boolean;
}) {
  const [show, setShow] = useState(false);
  return (
    <div className="field-row">
      <label>{label}</label>
      <div className="secret-field">
        {show ? (
          <VarInput className="auth-input" wrapClassName="grow" value={value} placeholder={placeholder} onChange={onChange} isVarSet={isVarSet} />
        ) : (
          <input className="auth-input" type="password" value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} />
        )}
        <button type="button" className="secret-toggle" title={show ? "隐藏" : "显示"} onClick={() => setShow((v) => !v)}>
          <EyeIcon off={!show} />
        </button>
      </div>
    </div>
  );
}

// 认证面板：顶部选方式 → 该方式的字段 → 底部「发送时实际附加什么」预览
function AuthPanel({
  d,
  set,
  isVarSet,
}: {
  d: ReqDraft;
  set: (patch: Partial<ReqDraft>) => void;
  isVarSet: (name: string) => boolean;
}) {
  const meta = AUTH_TYPE_METAS.find((m) => m.value === d.authType) ?? AUTH_TYPE_METAS[0];
  // 改认证配置无需手动清 token 缓存：执行内核的缓存键含 tokenUrl / clientId /
  // clientSecret / scope / clientAuth，任何一项变了就是另一把 token（见 core/src/auth.rs）
  const setAuth = (patch: Partial<ReqDraft>) => set(patch);
  const preview = authPreview(d);
  return (
    <div className="auth-panel">
      <div className="field-row auth-type-row">
        <label>认证方式</label>
        <Select
          className="field-select"
          value={d.authType}
          options={AUTH_TYPE_METAS.map((m) => ({ value: m.value, label: m.label }))}
          onChange={(v) => setAuth({ authType: v as AuthType })}
          ariaLabel="认证方式"
        />
        <span className="auth-type-zh">{meta.zh}</span>
      </div>

      {d.authType === "basic" && (
        <>
          <AuthText label="用户名" value={d.authBasicUser} onChange={(v) => setAuth({ authBasicUser: v })} placeholder="${{user}}" isVarSet={isVarSet} />
          <AuthSecret label="密码" value={d.authBasicPass} onChange={(v) => setAuth({ authBasicPass: v })} placeholder="${{password}}" isVarSet={isVarSet} />
        </>
      )}

      {d.authType === "bearer" && (
        <AuthText label="令牌" value={d.authBearerToken} onChange={(v) => setAuth({ authBearerToken: v })} placeholder="${{token}}" isVarSet={isVarSet} />
      )}

      {d.authType === "apikey" && (
        <>
          <AuthText label="键名" value={d.authApikeyKey} onChange={(v) => setAuth({ authApikeyKey: v })} placeholder="X-API-Key" isVarSet={isVarSet} />
          <AuthSecret label="值" value={d.authApikeyValue} onChange={(v) => setAuth({ authApikeyValue: v })} placeholder="${{apiKey}}" isVarSet={isVarSet} />
          <div className="field-row">
            <label>位置</label>
            <Select
              className="field-select"
              value={d.authApikeyIn}
              options={[
                { value: "header", label: "请求头（Header）" },
                { value: "query", label: "查询参数（Query）" },
              ]}
              onChange={(v) => setAuth({ authApikeyIn: v as "header" | "query" })}
            />
          </div>
        </>
      )}

      {d.authType === "digest" && (
        <>
          <AuthText label="用户名" value={d.authDigestUser} onChange={(v) => setAuth({ authDigestUser: v })} placeholder="${{user}}" isVarSet={isVarSet} />
          <AuthSecret label="密码" value={d.authDigestPass} onChange={(v) => setAuth({ authDigestPass: v })} placeholder="${{password}}" isVarSet={isVarSet} />
        </>
      )}

      {d.authType === "oauth2" && (
        <>
          <AuthText
            label="Token URL"
            value={d.authOauth2TokenUrl}
            onChange={(v) => setAuth({ authOauth2TokenUrl: v })}
            placeholder="https://auth.example.com/oauth/token"
            isVarSet={isVarSet}
          />
          <AuthText label="Client ID" value={d.authOauth2ClientId} onChange={(v) => setAuth({ authOauth2ClientId: v })} placeholder="${{clientId}}" isVarSet={isVarSet} />
          <AuthSecret
            label="Client Secret"
            value={d.authOauth2ClientSecret}
            onChange={(v) => setAuth({ authOauth2ClientSecret: v })}
            placeholder="${{clientSecret}}"
            isVarSet={isVarSet}
          />
          <AuthText label="Scope" value={d.authOauth2Scope} onChange={(v) => setAuth({ authOauth2Scope: v })} placeholder="可选，空格分隔" isVarSet={isVarSet} />
          <div className="field-row">
            <label>凭据位置</label>
            <Select
              className="field-select"
              value={d.authOauth2ClientAuth}
              options={[
                { value: "header", label: "Basic 请求头" },
                { value: "body", label: "表单体" },
              ]}
              onChange={(v) => setAuth({ authOauth2ClientAuth: v as "header" | "body" })}
            />
          </div>
        </>
      )}

      {preview && (
        <div className="auth-preview">
          <span className="auth-preview-label">{preview.label}</span>
          <code>{preview.code}</code>
        </div>
      )}
    </div>
  );
}

// 请求体面板：类型平铺成一排 chip（借 Apifox；不做 Postman 的 raw + 语言二级下拉——
// BodySpec.type 本就是平铺的，多一层映射只会凭空造概念），下面按类型给对应编辑区。
function BodyPanel({ d, set }: { d: ReqDraft; set: (patch: Partial<ReqDraft>) => void }) {
  const t = d.bodyType;
  const isTextual = t === "json" || t === "xml" || t === "text";
  const placeholder = t === "json" ? '{\n  "name": "apicase"\n}' : t === "xml" ? "<root>\n  <name>apicase</name>\n</root>" : "请求体文本";

  async function pickFile() {
    const picked = await open({ multiple: false, directory: false, title: "选择请求体文件" });
    if (typeof picked === "string") set({ bodyFilePath: picked });
  }

  return (
    <div className="body-panel">
      <div className="body-type-chips" role="radiogroup" aria-label="请求体类型">
        {BODY_TYPES.map((bt) => (
          <button
            key={bt.value}
            type="button"
            role="radio"
            aria-checked={t === bt.value}
            className={`body-type-chip ${t === bt.value ? "active" : ""}`}
            onClick={() => set({ bodyType: bt.value })}
          >
            {bt.label}
          </button>
        ))}
      </div>

      {/* Content-Type 行：text / binary 可改，其余给出实际会发的值 */}
      {t !== "none" && (
        <div className="body-ct-row">
          <span className="body-ct-label">Content-Type</span>
          {t === "text" || t === "binary" ? (
            <input
              className="ct-input"
              value={d.bodyContentType}
              placeholder={t === "text" ? DEFAULT_CONTENT_TYPE.text : d.bodyFilePath ? guessContentType(d.bodyFilePath) : "application/octet-stream"}
              onChange={(e) => set({ bodyContentType: e.target.value })}
            />
          ) : (
            <code className="body-ct-value">{t === "form-data" ? "multipart/form-data" : DEFAULT_CONTENT_TYPE[t]}</code>
          )}
        </div>
      )}

      {t === "none" && <div className="panel-hint">无请求体</div>}
      {isTextual && (
        <textarea className="body-input" value={d.bodyText} placeholder={placeholder} onChange={(e) => set({ bodyText: e.target.value })} />
      )}
      {(t === "form-urlencoded" || t === "form-data") && (
        <KVTable
          rows={d.bodyForm}
          onChange={(rows) => set({ bodyForm: rows })}
          namePlaceholder="字段名"
          valuePlaceholder="字段值"
          withDescription
          withType={t === "form-data"} // 只有 multipart 能带文件；urlencoded 一律文本
        />
      )}
      {t === "binary" && (
        <div className="binary-picker">
          <button type="button" className="file-pick-btn" onClick={pickFile}>
            选择文件…
          </button>
          {d.bodyFilePath ? (
            <>
              <code className="binary-path" title={d.bodyFilePath}>
                {d.bodyFilePath}
              </code>
              <button type="button" className="row-del" title="移除" onClick={() => set({ bodyFilePath: "" })}>
                <TrashIcon />
              </button>
            </>
          ) : (
            <span className="binary-empty">未选择文件</span>
          )}
        </div>
      )}
    </div>
  );
}

// 断言操作符的中文显示（存储仍用英文标识，保持 YAML 稳定）
export const OP_LABELS: Record<AssertOp, string> = {
  eq: "等于",
  ne: "不等于",
  contains: "包含",
  exists: "存在",
  notExists: "不存在",
  gt: "大于",
  lt: "小于",
  matches: "匹配",
};

// 表格恒有一行空白可填 —— **必须在渲染时算，不能只在编辑时追加**：
// 已保存的 case 载入时行行填满（序列化会丢掉空行，见 core/src/yaml/mod.rs），
// 环境变量 / 输出这类由父组件回写时也会过滤空行；只在 update 里 push 的话，
// 这些场景末尾就没有落笔的地方，用户加不了下一条（本函数即为此 bug 而生）。
export function kvRowsWithBlank(rows: FormItem[]): FormItem[] {
  const last = rows[rows.length - 1];
  const filled = !!last && !!(last.name || last.value || last.description || last.type);
  return !last || filled ? [...rows, { name: "", value: "", enabled: true }] : rows;
}

/** 同 kvRowsWithBlank，用于断言表 */
export function assertRowsWithBlank(rows: Assertion[]): Assertion[] {
  const last = rows[rows.length - 1];
  const filled = !!last && !!(last.target || last.value);
  return !last || filled ? [...rows, { target: "", op: "eq", value: "" }] : rows;
}

// 通用键值表格（query / headers / 表单项复用）：末行填写自动追加空行，每行可勾选启用。
// form-data 传 withType 多一列「类型」：该行可切文本 / 文件，文件行的值即本地文件路径。
export function KVTable({
  rows,
  onChange,
  namePlaceholder = "Key",
  valuePlaceholder = "Value",
  hideEnabled = false,
  withDescription = false,
  withType = false,
}: {
  rows: FormItem[];
  onChange: (rows: FormItem[]) => void;
  namePlaceholder?: string;
  valuePlaceholder?: string;
  hideEnabled?: boolean; // 无启用/停用语义的场景（如环境变量）隐藏勾选列
  withDescription?: boolean; // 多一列「描述」（数据模型支持 description 的场景，如参数/请求头/表单）
  withType?: boolean; // 多一列「类型」（仅 form-data：文本 / 文件）
}) {
  const display = kvRowsWithBlank(rows);
  function update(i: number, patch: Partial<FormItem>) {
    onChange(display.map((r, idx) => (idx === i ? { ...r, ...patch } : r)));
  }
  function remove(i: number) {
    onChange(display.filter((_, idx) => idx !== i));
  }
  // 文件行：值列即本地文件路径，改由系统文件选择框填
  async function pickFile(i: number) {
    const picked = await open({ multiple: false, directory: false, title: "选择上传文件" });
    if (typeof picked === "string") update(i, { value: picked });
  }
  return (
    <table className="kv-table grid">
      <thead>
        <tr>
          {!hideEnabled && <th className="ck-col"></th>}
          <th>名称</th>
          {withType && <th className="type-col">类型</th>}
          <th>值</th>
          {withDescription && <th>描述</th>}
          <th></th>
        </tr>
      </thead>
      <tbody>
        {display.map((r, i) => {
          const isFile = r.type === "file";
          const filled = !!(r.name || r.value || r.description || isFile);
          return (
            <tr key={i}>
              {!hideEnabled && (
                <td className="ck-col">
                  <input type="checkbox" checked={r.enabled !== false} onChange={(e) => update(i, { enabled: e.target.checked })} />
                </td>
              )}
              <td>
                <input value={r.name} placeholder={namePlaceholder} onChange={(e) => update(i, { name: e.target.value })} />
              </td>
              {withType && (
                <td className="type-col">
                  <Select
                    className="form-type-select"
                    value={isFile ? "file" : "text"}
                    options={[
                      { value: "text", label: "文本" },
                      { value: "file", label: "文件" },
                    ]}
                    ariaLabel="字段类型"
                    // 值的语义整个换了（文本 ↔ 路径），一并清空，免得把一段文本当路径发出去
                    onChange={(v) => update(i, { type: v === "file" ? "file" : undefined, value: "" })}
                  />
                </td>
              )}
              {isFile ? (
                <td className="file-cell">
                  <div className="cell-file">
                    <button type="button" className="cell-file-btn" onClick={() => pickFile(i)}>
                      选择文件…
                    </button>
                    {r.value ? (
                      <>
                        <code className="cell-file-path" title={r.value}>
                          {r.value}
                        </code>
                        <button type="button" className="row-del" title="移除文件" onClick={() => update(i, { value: "" })}>
                          <TrashIcon />
                        </button>
                      </>
                    ) : (
                      <span className="cell-file-empty">未选择文件</span>
                    )}
                  </div>
                </td>
              ) : (
                <td>
                  <input value={r.value} placeholder={valuePlaceholder} onChange={(e) => update(i, { value: e.target.value })} />
                </td>
              )}
              {withDescription && (
                <td>
                  <input value={r.description || ""} placeholder="描述" onChange={(e) => update(i, { description: e.target.value })} />
                </td>
              )}
              <td className="op-cell">
                {filled && (
                  <button className="row-del" title="删除" onClick={() => remove(i)}>
                    <TrashIcon />
                  </button>
                )}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

// 断言目标输入框：带路径补全。候选与当前值都来自最近一次响应（见 assertPath.ts）——
// 路径取不取得到，选之前就看得见，不必跑一遍再对着 ∅ 猜是"服务端没返"还是"我写错了"。
function TargetInput({ value, onChange, resp }: { value: string; onChange: (v: string) => void; resp?: RespLite }) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [box, setBox] = useState<{ left: number; top: number; width: number } | null>(null);
  const ref = useRef<HTMLInputElement>(null);
  const items = open ? suggestTargets(value, resp) : [];
  const at = Math.min(active, Math.max(items.length - 1, 0));

  // 浮层 fixed 锚在输入框下沿：断言表在 .req-scroll 里，绝对定位会被滚动容器裁掉（同 .ctx-menu）
  function show() {
    const r = ref.current?.getBoundingClientRect();
    if (r) setBox({ left: r.left, top: r.bottom + 2, width: Math.max(r.width, 260) });
    setOpen(true);
  }
  function accept(s: Suggestion) {
    onChange(s.more ? s.text + "." : s.text);
    setActive(0);
    setOpen(s.more); // 还有下一层就接着提示，一路点到底不用手打分隔符；到叶子就收起
    ref.current?.focus();
  }
  function onKeyDown(e: KeyEvent<HTMLInputElement>) {
    if (e.key === "Escape") {
      if (open) e.stopPropagation(); // 面板开着时 Esc 只收面板，不外传给全局快捷键
      setOpen(false);
      return;
    }
    if (!items.length) {
      if (e.key === "ArrowDown") show();
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setActive((at + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length);
    } else if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      accept(items[at]);
    }
  }
  return (
    <div className="path-input">
      <input
        ref={ref}
        value={value}
        placeholder="res.status / res.body.data.token"
        onChange={(e) => {
          onChange(e.target.value);
          setActive(0);
          show();
        }}
        onFocus={show}
        onClick={show}
        onBlur={() => setOpen(false)}
        onKeyDown={onKeyDown}
      />
      {open && items.length > 0 && box && (
        <div className="path-menu" style={{ left: box.left, top: box.top, width: box.width }}>
          {items.map((s, i) => (
            <button
              type="button"
              key={s.text}
              className={`path-item ${i === at ? "is-active" : ""}`}
              // 必须拦下 mousedown：否则输入框先失焦、面板已关，click 落空
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => accept(s)}
            >
              <span className="path-label">{s.label}</span>
              {s.hint && <span className="path-hint">{s.hint}</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// 期望值输入框：空着时把 target 的当前值摆在 placeholder 上（`当前：200`），
// 聚焦后一条候选，点击 / 回车填入。**只建议、不自动填**——把返回值一键固化成期望值，
// 很容易做出下次必红的脆弱断言，那一下得由人来点。
function ValueInput({ value, onChange, suggestion }: { value: string; onChange: (v: string) => void; suggestion: string | null }) {
  const [open, setOpen] = useState(false);
  const [box, setBox] = useState<{ left: number; top: number; width: number } | null>(null);
  const ref = useRef<HTMLInputElement>(null);
  const has = suggestion !== null && !value; // 已经填了就别打扰

  function show() {
    if (!has) return;
    const r = ref.current?.getBoundingClientRect();
    if (r) setBox({ left: r.left, top: r.bottom + 2, width: Math.max(r.width, 200) });
    setOpen(true);
  }
  function accept() {
    if (suggestion === null) return;
    onChange(suggestion);
    setOpen(false);
    ref.current?.focus();
  }
  return (
    <div className="path-input">
      <input
        ref={ref}
        value={value}
        placeholder={has ? `当前：${suggestion}` : "期望值"}
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(false); // 一开始打字就说明不要这条建议了
        }}
        onFocus={show}
        onClick={show}
        onBlur={() => setOpen(false)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            if (open) e.stopPropagation();
            setOpen(false);
          } else if (open && (e.key === "Enter" || e.key === "Tab")) {
            e.preventDefault();
            accept();
          }
        }}
      />
      {open && has && box && (
        <div className="path-menu" style={{ left: box.left, top: box.top, width: box.width }}>
          <button type="button" className="path-item is-active" onMouseDown={(e) => e.preventDefault()} onClick={accept}>
            <span className="path-label">{suggestion}</span>
            <span className="path-hint">当前值</span>
          </button>
        </div>
      )}
    </div>
  );
}

// 断言表：目标 / 操作符 / 期望值（仅配置；运行结果在响应区「断言」栏展示）
// 视觉与参数/请求头表一致——同一套 .kv-table.grid Excel 网格
function AssertTable({
  rows,
  onChange,
  resp,
}: {
  rows: Assertion[];
  onChange: (rows: Assertion[]) => void;
  resp?: RespLite; // 最近一次响应：给目标列的路径补全当数据源
}) {
  const display = assertRowsWithBlank(rows);
  function update(i: number, patch: Partial<Assertion>) {
    onChange(display.map((r, idx) => (idx === i ? { ...r, ...patch } : r)));
  }
  function remove(i: number) {
    onChange(display.filter((_, idx) => idx !== i));
  }
  return (
    <table className="kv-table grid assert-table">
      <thead>
        <tr>
          <th>目标</th>
          <th className="op-col2">断言</th>
          <th>期望值</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {display.map((r, i) => {
          const noVal = r.op === "exists" || r.op === "notExists";
          return (
            <tr key={i}>
              <td>
                <TargetInput value={r.target} onChange={(v) => update(i, { target: v })} resp={resp} />
              </td>
              <td className="op-col2">
                <Select
                  className="assert-op-select"
                  value={r.op}
                  options={ASSERT_OPS.map((op) => ({ value: op, label: OP_LABELS[op] }))}
                  onChange={(v) => update(i, { op: v as AssertOp })}
                />
              </td>
              {/* exists / notExists 无需期望值：单元格置灰示意不可填 */}
              <td className={noVal ? "na-cell" : ""}>
                {!noVal && (
                  <ValueInput
                    value={r.value || ""}
                    onChange={(v) => update(i, { value: v })}
                    suggestion={suggestValue(r.target, r.op, resp)}
                  />
                )}
              </td>
              <td className="op-cell">
                {r.target && (
                  <button className="row-del" title="删除" onClick={() => remove(i)}>
                    <TrashIcon />
                  </button>
                )}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

// 请求 ID 输入：本地编辑、失焦/回车提交（避免逐键改 id 破坏引用）
function StepIdField({ id, onCommit }: { id: string; onCommit: (v: string) => void }) {
  const [v, setV] = useState(id);
  useEffect(() => {
    setV(id);
  }, [id]);
  return (
    <input
      className="sm-id-input"
      value={v}
      onChange={(e) => setV(e.target.value)}
      onBlur={() => {
        if (v.trim() && v.trim() !== id) onCommit(v.trim());
        else setV(id);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        else if (e.key === "Escape") {
          setV(id);
          (e.target as HTMLInputElement).blur();
        }
      }}
    />
  );
}

export function RequestEditor({
  value,
  onChange,
  onSend,
  sending,
  sendLabel = "发送",
  assertions,
  onAssertions,
  outputs,
  onOutputs,
  docs,
  onDocs,
  stepId,
  onRenameId,
  protocol,
  onProtocol,
  isVarSet = () => true,
  resp,
}: {
  value: ReqDraft;
  onChange: (d: ReqDraft) => void;
  onSend: () => void;
  sending: boolean;
  sendLabel?: string;
  assertions?: Assertion[];
  onAssertions?: (a: Assertion[]) => void;
  outputs?: RequestOutput[];
  onOutputs?: (o: RequestOutput[]) => void;
  docs?: string;
  onDocs?: (v: string) => void;
  stepId?: string;
  onRenameId?: (newId: string) => void;
  protocol?: string; // 请求协议（当前仅 http）
  onProtocol?: (p: string) => void;
  isVarSet?: (name: string) => boolean; // 判断某 {{变量}} 在当前环境是否已设值（用于 URL 高亮）
  resp?: RespLite; // 本 step 最近一次响应：断言目标列按它的真实结构给补全候选
}) {
  const [tab, setTab] = useState<string>("params");
  const d = value;
  const set = (patch: Partial<ReqDraft>) => onChange({ ...d, ...patch });

  const onUrlChange = (raw: string) => set({ url: raw, query: splitQueryFromUrl(raw).query });
  const onQueryChange = (next: KV[]) => set({ query: next, url: mergeQueryIntoUrl(d.url, next) });

  const paramCount = d.query.filter((q) => q.enabled !== false && (q.name || q.value)).length;
  const headerCount = d.headers.filter((h) => h.enabled !== false && (h.name || h.value)).length;
  const outputCount = (outputs || []).filter((o) => o.name).length;
  const assertCount = (assertions || []).filter((a) => a.target).length;

  const tabs: string[] = ["params", "headers", "auth", "body"];
  if (onOutputs) tabs.push("outputs");
  if (onAssertions) tabs.push("assert");
  if (onDocs) tabs.push("docs");
  if (onRenameId) tabs.push("meta");
  const label = (t: string) =>
    t === "params"
      ? "参数"
      : t === "headers"
        ? "请求头"
        : t === "auth"
          ? "认证"
          : t === "body"
            ? "请求体"
            : t === "outputs"
              ? "输出"
              : t === "assert"
                ? "断言"
                : t === "docs"
                  ? "文档"
                  : "属性";
  const tabBadge = (t: string) =>
    t === "params" ? paramCount : t === "headers" ? headerCount : t === "outputs" ? outputCount : t === "assert" ? assertCount : 0;

  return (
    <div className="req-editor">
      {/* 请求行 */}
      <div className="request-bar">
        <div className="url-group">
          <Select
            className={`method-select ${methodClass(d.method)}`}
            value={d.method}
            options={METHODS.map((m) => ({ value: m, label: m }))}
            onChange={(v) => set({ method: v })}
            ariaLabel="请求方法"
            optionClassName={methodClass}
          />
          <VarInput
            className="url-input"
            wrapClassName="grow"
            value={d.url}
            placeholder="https://api.example.com/path"
            onChange={onUrlChange}
            onKeyDown={(e) => {
              if (e.key === "Enter") onSend();
            }}
            isVarSet={isVarSet}
          />
        </div>
        <button className="send-btn" onClick={onSend} disabled={sending}>
          {sending ? "发送中…" : sendLabel}
        </button>
      </div>

      {/* 请求配置 Tabs */}
      <div className="tabs">
        {tabs.map((t) => (
          <button key={t} className={`tab ${tab === t ? "active" : ""}`} onClick={() => setTab(t)}>
            {label(t)}
            {tabBadge(t) > 0 && <span className="tab-count">{tabBadge(t)}</span>}
          </button>
        ))}
      </div>

      <div className="tab-panel">
        {tab === "params" && <KVTable rows={d.query} onChange={onQueryChange} namePlaceholder="参数名" valuePlaceholder="参数值" withDescription />}
        {tab === "headers" && (
          <KVTable rows={d.headers} onChange={(rows) => set({ headers: rows })} namePlaceholder="请求头名称" valuePlaceholder="值" withDescription />
        )}
        {tab === "auth" && <AuthPanel d={d} set={set} isVarSet={isVarSet} />}
        {tab === "body" && <BodyPanel d={d} set={set} />}
        {tab === "outputs" && onOutputs && (
          <div className="outputs-panel">
            <div className="panel-hint">从响应提取变量，供下游请求 <code>{"{{requests.本请求.outputs.变量名}}"}</code> 引用。</div>
            <KVTable
              rows={(outputs || []).map((o) => ({ name: o.name, value: o.path, enabled: true }))}
              onChange={(rows) => onOutputs(rows.filter((r) => r.name || r.value).map((r) => ({ name: r.name, path: r.value })))}
              namePlaceholder="变量名"
              valuePlaceholder="JSONPath 如 $.data.token"
            />
          </div>
        )}
        {tab === "assert" && onAssertions && (
          <div className="assert-panel">
            <div className="panel-hint">
              目标统一以 <code>res</code> 开头：<code>res.status</code> / <code>res.body.data.token</code> / <code>res.headers.名称</code>；
              目标格输入时按最近一次响应逐层提示。运行结果见响应区「断言」栏。
            </div>
            <AssertTable rows={assertions || []} onChange={onAssertions} resp={resp} />
          </div>
        )}
        {tab === "docs" && onDocs && (
          <div className="docs-panel">
            <MarkdownEditor value={docs || ""} onChange={onDocs} compact placeholder="为该请求编写 Markdown 文档：用途、参数说明、注意事项…" />
          </div>
        )}
        {tab === "meta" && onRenameId && (
          <div className="meta-panel">
            <div className="panel-hint">请求在用例中的唯一标识与通信协议，用于流程编排与变量引用。</div>
            <div className="field-row">
              <label>id</label>
              <StepIdField id={stepId || ""} onCommit={onRenameId} />
            </div>
            {onProtocol && (
              <div className="field-row">
                <label>协议</label>
                <Select
                  className="field-select"
                  value={protocol || "http"}
                  options={PROTOCOLS.map((p) => ({ value: p, label: p }))}
                  onChange={onProtocol}
                />
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
