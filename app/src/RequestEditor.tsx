// 通用请求编辑器：单请求 case 与多请求 flow 的每个请求都复用它。
// 完全受控——父组件持有 ReqDraft，本组件只读 value、通过 onChange 汇报变更。
// 切换 step 时父组件用 key 强制重挂载，从而重置内部 Tab 等瞬时状态。
import { useEffect, useRef, useState } from "react";
import { KV, AuthType, BodyType, Assertion, AssertOp, ASSERT_OPS, RequestOutput, splitQueryFromUrl, mergeQueryIntoUrl } from "./case";
import { ReqDraft } from "./draft";
import { AssertResult } from "./flow";

export const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const BODY_TYPES: BodyType[] = ["none", "json", "text", "form-urlencoded", "form-data"];
const AUTH_TYPES: AuthType[] = ["none", "bearer", "basic", "apikey"];

export function methodClass(m: string): string {
  return `method-${m.toLowerCase()}`;
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
}: {
  value: string;
  options: SelectOption[];
  onChange: (v: string) => void;
  className?: string;
  ariaLabel?: string;
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
  return (
    <div className={`ui-select ${open ? "is-open" : ""} ${className}`} ref={ref}>
      <button type="button" className="ui-select-trigger" aria-label={ariaLabel} onClick={() => setOpen((v) => !v)}>
        <span className="ui-select-value">{current?.label ?? value}</span>
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
              className={`ui-select-option ${o.value === value ? "is-active" : ""}`}
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

// 断言操作符的中文显示（存储仍用英文标识，保持 YAML 稳定）
const OP_LABELS: Record<AssertOp, string> = {
  eq: "等于",
  ne: "不等于",
  contains: "包含",
  exists: "存在",
  notExists: "不存在",
  gt: "大于",
  lt: "小于",
  matches: "匹配",
};

// 通用键值表格（query / headers / 表单项复用）：末行填写自动追加空行，每行可勾选启用
export function KVTable({
  rows,
  onChange,
  namePlaceholder = "Key",
  valuePlaceholder = "Value",
}: {
  rows: KV[];
  onChange: (rows: KV[]) => void;
  namePlaceholder?: string;
  valuePlaceholder?: string;
}) {
  const display = rows.length ? rows : [{ name: "", value: "", enabled: true }];
  function update(i: number, patch: Partial<KV>) {
    const next = display.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    const last = next[next.length - 1];
    if (last.name || last.value) next.push({ name: "", value: "", enabled: true });
    onChange(next);
  }
  function remove(i: number) {
    onChange(display.filter((_, idx) => idx !== i));
  }
  return (
    <table className="kv-table">
      <thead>
        <tr>
          <th className="ck-col"></th>
          <th>键</th>
          <th>值</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {display.map((r, i) => {
          const filled = !!(r.name || r.value);
          return (
            <tr key={i}>
              <td className="ck-col">
                <input type="checkbox" checked={r.enabled !== false} onChange={(e) => update(i, { enabled: e.target.checked })} />
              </td>
              <td>
                <input value={r.name} placeholder={namePlaceholder} onChange={(e) => update(i, { name: e.target.value })} />
              </td>
              <td>
                <input value={r.value} placeholder={valuePlaceholder} onChange={(e) => update(i, { value: e.target.value })} />
              </td>
              <td className="op-cell">
                {filled && (
                  <button className="row-del" onClick={() => remove(i)}>
                    ×
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

// 断言表：目标 / 操作符 / 期望值 + 运行后逐条 ✓/✗
function AssertTable({
  rows,
  onChange,
  results,
}: {
  rows: Assertion[];
  onChange: (rows: Assertion[]) => void;
  results?: AssertResult[];
}) {
  const display: Assertion[] = rows.length ? rows : [{ target: "", op: "eq", value: "" }];
  function update(i: number, patch: Partial<Assertion>) {
    const next = display.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    const last = next[next.length - 1];
    if (last.target) next.push({ target: "", op: "eq", value: "" });
    onChange(next);
  }
  function remove(i: number) {
    onChange(display.filter((_, idx) => idx !== i));
  }
  return (
    <table className="kv-table assert-table">
      <thead>
        <tr>
          <th className="res-col"></th>
          <th>目标</th>
          <th className="op-col2">断言</th>
          <th>期望值</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {display.map((r, i) => {
          const res = r.target ? results?.[i] : undefined;
          const noVal = r.op === "exists" || r.op === "notExists";
          return (
            <tr key={i}>
              <td className="res-col">
                {res && (
                  <span className={`assert-badge ${res.ok ? "ok" : "bad"}`} title={`实际：${res.actual}`}>
                    {res.ok ? "✓" : "✗"}
                  </span>
                )}
              </td>
              <td>
                <input value={r.target} placeholder="status / $.data.token / header.X" onChange={(e) => update(i, { target: e.target.value })} />
              </td>
              <td className="op-col2">
                <Select
                  className="assert-op-select"
                  value={r.op}
                  options={ASSERT_OPS.map((op) => ({ value: op, label: OP_LABELS[op] }))}
                  onChange={(v) => update(i, { op: v as AssertOp })}
                />
              </td>
              <td>
                {!noVal && <input value={r.value || ""} placeholder="期望值" onChange={(e) => update(i, { value: e.target.value })} />}
              </td>
              <td className="op-cell">
                {r.target && (
                  <button className="row-del" onClick={() => remove(i)}>
                    ×
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
  assertResults,
  outputs,
  onOutputs,
  stepId,
  onRenameId,
}: {
  value: ReqDraft;
  onChange: (d: ReqDraft) => void;
  onSend: () => void;
  sending: boolean;
  sendLabel?: string;
  assertions?: Assertion[];
  onAssertions?: (a: Assertion[]) => void;
  assertResults?: AssertResult[];
  outputs?: RequestOutput[];
  onOutputs?: (o: RequestOutput[]) => void;
  stepId?: string;
  onRenameId?: (newId: string) => void;
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
  const assertPass = (assertResults || []).filter((r) => r.ok).length;

  const tabs: string[] = ["params", "headers", "auth", "body"];
  if (onOutputs) tabs.push("outputs");
  if (onAssertions) tabs.push("assert");
  if (onRenameId) tabs.push("meta");
  const label = (t: string) =>
    t === "params" ? "参数" : t === "headers" ? "请求头" : t === "auth" ? "认证" : t === "body" ? "请求体" : t === "outputs" ? "输出" : t === "assert" ? "断言" : "请求 ID";
  const tabBadge = (t: string) => (t === "params" ? paramCount : t === "headers" ? headerCount : t === "outputs" ? outputCount : 0);

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
          />
          <input
            className="url-input"
            value={d.url}
            placeholder="https://api.example.com/path"
            onChange={(e) => onUrlChange(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") onSend();
            }}
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
            {t === "assert" && assertCount > 0 && (
              <span className={`tab-count ${assertResults && assertResults.length ? (assertPass === assertResults.length ? "all-ok" : "has-bad") : ""}`}>
                {assertResults && assertResults.length ? `${assertPass}/${assertResults.length}` : assertCount}
              </span>
            )}
          </button>
        ))}
      </div>

      <div className="tab-panel">
        {tab === "params" && <KVTable rows={d.query} onChange={onQueryChange} namePlaceholder="参数名" valuePlaceholder="参数值" />}
        {tab === "headers" && (
          <KVTable rows={d.headers} onChange={(rows) => set({ headers: rows })} namePlaceholder="请求头名称" valuePlaceholder="值" />
        )}
        {tab === "auth" && (
          <div className="auth-panel">
            <div className="field-row">
              <label>类型</label>
              <Select
                className="field-select"
                value={d.authType}
                options={AUTH_TYPES.map((t) => ({ value: t, label: t }))}
                onChange={(v) => set({ authType: v as AuthType })}
              />
            </div>
            {d.authType === "bearer" && (
              <div className="field-row">
                <label>令牌</label>
                <input value={d.authBearerToken} placeholder="{{token}}" onChange={(e) => set({ authBearerToken: e.target.value })} />
              </div>
            )}
            {d.authType === "basic" && (
              <>
                <div className="field-row">
                  <label>用户名</label>
                  <input value={d.authBasicUser} onChange={(e) => set({ authBasicUser: e.target.value })} />
                </div>
                <div className="field-row">
                  <label>密码</label>
                  <input value={d.authBasicPass} onChange={(e) => set({ authBasicPass: e.target.value })} />
                </div>
              </>
            )}
            {d.authType === "apikey" && (
              <>
                <div className="field-row">
                  <label>键名</label>
                  <input value={d.authApikeyKey} onChange={(e) => set({ authApikeyKey: e.target.value })} />
                </div>
                <div className="field-row">
                  <label>值</label>
                  <input value={d.authApikeyValue} onChange={(e) => set({ authApikeyValue: e.target.value })} />
                </div>
                <div className="field-row">
                  <label>位置</label>
                  <Select
                    className="field-select"
                    value={d.authApikeyIn}
                    options={[
                      { value: "header", label: "header" },
                      { value: "query", label: "query" },
                    ]}
                    onChange={(v) => set({ authApikeyIn: v as "header" | "query" })}
                  />
                </div>
              </>
            )}
            {d.authType === "none" && <div className="panel-hint">无认证</div>}
          </div>
        )}
        {tab === "body" && (
          <div className="body-panel">
            <div className="body-type-bar">
              <Select
                className="bodytype-select"
                value={d.bodyType}
                options={BODY_TYPES.map((t) => ({ value: t, label: t }))}
                onChange={(v) => set({ bodyType: v as BodyType })}
              />
              {d.bodyType === "text" && (
                <input
                  className="ct-input"
                  value={d.bodyContentType}
                  placeholder="Content-Type（可选）"
                  onChange={(e) => set({ bodyContentType: e.target.value })}
                />
              )}
            </div>
            {d.bodyType === "none" && <div className="panel-hint">无请求体</div>}
            {(d.bodyType === "json" || d.bodyType === "text") && (
              <textarea
                className="body-input"
                value={d.bodyText}
                placeholder={d.bodyType === "json" ? '{"name":"apicase"}' : "请求体文本"}
                onChange={(e) => set({ bodyText: e.target.value })}
              />
            )}
            {(d.bodyType === "form-urlencoded" || d.bodyType === "form-data") && (
              <KVTable rows={d.bodyForm} onChange={(rows) => set({ bodyForm: rows })} namePlaceholder="字段名" valuePlaceholder="字段值" />
            )}
            {d.bodyType === "form-data" && <div className="panel-hint">form-data 发送暂仅支持文本字段</div>}
          </div>
        )}
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
              目标：<code>status</code> / JSONPath（<code>$.data.token</code>）/ <code>header.名称</code>；运行后逐条校验。
            </div>
            <AssertTable rows={assertions || []} onChange={onAssertions} results={assertResults} />
          </div>
        )}
        {tab === "meta" && onRenameId && (
          <div className="meta-panel">
            <div className="panel-hint">请求在用例中的唯一标识，用于流程编排与变量引用。</div>
            <div className="field-row">
              <label>请求 ID</label>
              <StepIdField id={stepId || ""} onCommit={onRenameId} />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
