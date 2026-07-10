import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Case, Request, RequestOutput, Assertion, UiNodes, analyzeCase, dumpCase, splitQueryFromUrl, parseEnvironments, dumpApplicationConfig } from "./case";
import { ReqDraft, requestToDraft, draftToRequest, buildApiRequest, emptyDraft } from "./draft";
import { RequestEditor, KVTable, METHODS, methodClass, Select } from "./RequestEditor";
import { FlowCanvas, FlowNode } from "./FlowCanvas";
import { TerminalPane } from "./TerminalPane";
import { AiChat } from "./AiChat";
import { RunContext, AssertResult, resolveDraft, extractOutputs, evalAssertions } from "./flow";
import {
  ACTIONS,
  ACTION_MAP,
  type ActionDef,
  type ActionId,
  type Overrides,
  eventToAccel,
  accelKey,
  formatAccel,
  accelTokens,
  resolveBindings,
  buildLookup,
  findConflict,
  isDefaultBinding,
  loadOverrides,
  saveOverrides,
} from "./shortcuts";
import "./App.css";

interface HeaderEntry {
  key: string;
  value: string;
}

interface ApiResponse {
  status: number;
  statusText: string;
  headers: HeaderEntry[];
  body: string;
  elapsedMs: number;
}

interface DirEntry {
  name: string;
  path: string;
  isDir: boolean;
}

// case 内部统一模型：单请求 = 只有 1 个请求；每个请求复用 ReqDraft
interface RequestDraft {
  id: string;
  dependsOn: string[];
  outputs: RequestOutput[];
  assertions: Assertion[];
  req: ReqDraft;
}

interface RunState {
  status: "idle" | "running" | "ok" | "err";
  resp?: ApiResponse | null;
  error?: string | null;
  asserts?: AssertResult[];
}

// 一个标签页的完整编辑态快照（切换/后台保存时用）
interface TabSnapshot {
  path: string;
  caseName: string;
  caseVars: Record<string, unknown> | undefined;
  caseVersion: string;
  dirty: boolean;
  requests: RequestDraft[];
  selectedRequestId: string;
  uiNodes: UiNodes | undefined;
  textMode: boolean;
  showFlow: boolean;
  showRequest: boolean;
  rawText: string;
  caseValid: boolean;
  textError: string | null;
  binaryFile: boolean;
  configVisual: boolean;
  runMap: Record<string, RunState>;
  outputsCtx: Record<string, Record<string, unknown>>;
  respTab: "body" | "headers";
  pretty: boolean;
  error: string | null;
}

function statusClass(status: number): string {
  if (status >= 200 && status < 300) return "status-2xx";
  if (status >= 300 && status < 400) return "status-3xx";
  if (status >= 400 && status < 500) return "status-4xx";
  return "status-5xx";
}

function prettyBody(body: string): string {
  try {
    return JSON.stringify(JSON.parse(body), null, 2);
  } catch {
    return body;
  }
}

function baseName(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] || p;
}

function dirName(p: string): string {
  const idx = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return idx <= 0 ? p : p.slice(0, idx);
}

function joinPath(dir: string, name: string): string {
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.endsWith(sep) ? dir + name : dir + sep + name;
}

// 相对工作空间根的路径（搜索结果展示用）
function relPath(root: string, p: string): string {
  if (p.startsWith(root)) {
    return p.slice(root.length).replace(/^[\\/]+/, "") || baseName(p);
  }
  return p;
}

// 可在编辑器打开的 yml/yaml（case 渲染结构化，其余落文本）
function isYamlFile(path: string): boolean {
  const n = baseName(path).toLowerCase();
  return n.endsWith(".yml") || n.endsWith(".yaml");
}

// 工作空间根配置文件
function isAppConfig(path: string): boolean {
  const n = baseName(path).toLowerCase();
  return n === "application.yml" || n === "application.yaml";
}

// 已知二进制/媒体扩展名：直接短路，不读取整个文件（避免大文件读入内存）
const BINARY_EXTS = new Set([
  // 图片
  "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff", "heic", "avif",
  // 音频
  "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus",
  // 视频
  "mp4", "mov", "avi", "mkv", "webm", "flv", "wmv", "m4v",
  // 压缩包
  "zip", "tar", "gz", "tgz", "bz2", "xz", "rar", "7z",
  // 文档/办公
  "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
  // 字体
  "ttf", "otf", "woff", "woff2", "eot",
  // 可执行/库/数据库/其它
  "exe", "dll", "so", "dylib", "bin", "class", "jar", "wasm", "sqlite", "db",
]);
function isBinaryExt(path: string): boolean {
  const n = baseName(path).toLowerCase();
  const i = n.lastIndexOf(".");
  return i > 0 && BINARY_EXTS.has(n.slice(i + 1));
}

// ── 文件树图标（SVG，currentColor 由 CSS 控色）──
function FolderIcon() {
  return (
    <svg className="tree-ico ico-folder" viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
      <path
        fill="currentColor"
        d="M1.75 4c0-.69.56-1.25 1.25-1.25h3.09c.4 0 .78.19 1.02.51l.63.84c.05.06.12.1.2.1h5.06c.69 0 1.25.56 1.25 1.25v6.05c0 .69-.56 1.25-1.25 1.25H3c-.69 0-1.25-.56-1.25-1.25z"
      />
    </svg>
  );
}
function FileIcon({ active }: { active?: boolean }) {
  return (
    <svg className={`tree-ico ico-file ${active ? "is-case" : ""}`} viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
      <path fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" d="M4.25 2h4.5l3 3v8.75a.25.25 0 0 1-.25.25H4.25a.25.25 0 0 1-.25-.25V2.25A.25.25 0 0 1 4.25 2z" />
      <path fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" d="M8.5 2.25V5h2.75" />
    </svg>
  );
}
// 工作空间配置文件（application.yml）专用：齿轮图标，一眼可辨"这是配置文件"
// viewBox 24×24 且齿轮四周留约 2.5 边距，缩放渲染时齿尖不会被裁切；可复用到顶栏配置按钮
function ConfigIcon({ className = "tree-ico ico-config", size = 15 }: { className?: string; size?: number }) {
  return (
    <svg className={className} viewBox="0 0 24 24" width={size} height={size} aria-hidden="true">
      <path
        fill="currentColor"
        fillRule="evenodd"
        d="M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58c.18-.14.23-.41.12-.61l-1.92-3.32c-.12-.22-.37-.29-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54c-.04-.24-.24-.41-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58c-.18.14-.23.41-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z"
      />
    </svg>
  );
}
// 展开/折叠 chevron（默认指向右，展开时旋转 90° 指向下，带过渡）
function Chevron({ open }: { open: boolean }) {
  return (
    <svg className={`chevron ${open ? "is-open" : ""}`} viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
      <path fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" d="M6 3.5 10.5 8 6 12.5" />
    </svg>
  );
}
// 布局切换图标（仿 VSCode）：外框方块 + 对应一侧填充块；`on` 时高亮激活。
// side 决定填充块位置：left=左列、bottom=底行、right=右列。
function PanelIcon({ side }: { side: "left" | "bottom" | "right" }) {
  // 内部填充块的位置（16×16 视图，外框内边距 2）
  const fill =
    side === "left"
      ? { x: 2.5, y: 2.5, width: 4.5, height: 11 }
      : side === "right"
      ? { x: 9, y: 2.5, width: 4.5, height: 11 }
      : { x: 2.5, y: 9.5, width: 11, height: 4 };
  return (
    <svg className="panel-ico" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
      <rect x="1.5" y="2.5" width="13" height="11" rx="1.6" fill="none" stroke="currentColor" strokeWidth="1.2" />
      <rect {...fill} rx="0.6" fill="currentColor" className="panel-ico-fill" />
    </svg>
  );
}
// 下拉指示 caret（指向下，展开时旋转 180° 指向上）
function CaretDown({ open }: { open: boolean }) {
  return (
    <svg className={`caret-down ${open ? "is-open" : ""}`} viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <path fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" d="M4 6 8 10 12 6" />
    </svg>
  );
}

function byteSize(s: string): string {
  const bytes = new Blob([s]).size;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

// Case → 内部 RequestDraft 列表（case.requests 已是统一列表；空则兜底 1 个）
function caseToRequests(c: Case): { requests: RequestDraft[]; ui?: UiNodes } {
  const src = c.requests.length
    ? c.requests
    : [{ id: "req1", http: emptyDraftRequest(), dependsOn: [], outputs: [], assertions: [] }];
  return {
    requests: src.map((r) => ({
      id: r.id,
      dependsOn: r.dependsOn,
      outputs: r.outputs,
      assertions: r.assertions,
      req: requestToDraft(r.http),
    })),
    ui: c.ui?.nodes,
  };
}

// 极少数：valid 但 http 报文缺失时的兜底
function emptyDraftRequest() {
  return { method: "GET", url: "", query: [], headers: [], auth: { type: "none" as const }, body: { type: "none" as const } };
}

// 拓扑序（运行时按依赖先后逐个请求执行）
function topoOrder(sds: RequestDraft[]): RequestDraft[] {
  const byId = new Map(sds.map((s) => [s.id, s]));
  const visited = new Set<string>();
  const out: RequestDraft[] = [];
  const visit = (s: RequestDraft, stack: Set<string>) => {
    if (visited.has(s.id) || stack.has(s.id)) return;
    stack.add(s.id);
    for (const dep of s.dependsOn) {
      const d = byId.get(dep);
      if (d) visit(d, stack);
    }
    stack.delete(s.id);
    visited.add(s.id);
    out.push(s);
  };
  for (const s of sds) visit(s, new Set());
  return out;
}

// 文件树节点（递归渲染，支持展开/折叠 + 右键菜单）
function TreeNode({
  entry,
  depth,
  expanded,
  childrenMap,
  selectedPath,
  onToggle,
  onSelect,
  onContext,
}: {
  entry: DirEntry;
  depth: number;
  expanded: Set<string>;
  childrenMap: Record<string, DirEntry[]>;
  selectedPath: string;
  onToggle: (entry: DirEntry) => void;
  onSelect: (path: string) => void;
  onContext: (e: React.MouseEvent, entry: DirEntry) => void;
}) {
  const isOpen = expanded.has(entry.path);
  const children = childrenMap[entry.path];
  const isSelected = selectedPath === entry.path;
  const rowRef = useRef<HTMLDivElement>(null);
  // 成为选中项时（含展开后异步挂载）滚动到可见范围，最小滚动、不影响横向
  useEffect(() => {
    if (isSelected) rowRef.current?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [isSelected]);
  return (
    <div className="tree-node">
      <div
        ref={rowRef}
        className={`tree-row ${isSelected ? "selected" : ""}`}
        style={{ paddingLeft: 6 + depth * 14 }}
        title={entry.name}
        onClick={() => (entry.isDir ? onToggle(entry) : onSelect(entry.path))}
        onContextMenu={(e) => onContext(e, entry)}
      >
        <span className={`tree-caret ${entry.isDir ? "" : "tree-caret-empty"}`}>{entry.isDir && <Chevron open={isOpen} />}</span>
        {entry.isDir ? <FolderIcon /> : isAppConfig(entry.path) ? <ConfigIcon /> : <FileIcon active={isYamlFile(entry.path)} />}
        <span className="tree-name">{entry.name}</span>
      </div>
      {entry.isDir && isOpen && children && (
        <div className="tree-children">
          {children.map((c) => (
            <TreeNode
              key={c.path}
              entry={c}
              depth={depth + 1}
              expanded={expanded}
              childrenMap={childrenMap}
              selectedPath={selectedPath}
              onToggle={onToggle}
              onSelect={onSelect}
              onContext={onContext}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// 右键菜单
function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: { label: string; onClick: () => void; danger?: boolean }[];
  onClose: () => void;
}) {
  useEffect(() => {
    function close() {
      onClose();
    }
    document.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      document.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [onClose]);
  return (
    <div className="ctx-menu" style={{ left: x, top: y }} onMouseDown={(e) => e.stopPropagation()}>
      {items.map((it, i) => (
        <button
          key={i}
          className={`ctx-item ${it.danger ? "danger" : ""}`}
          onClick={() => {
            onClose();
            it.onClick();
          }}
        >
          {it.label}
        </button>
      ))}
    </div>
  );
}

// 文本输入对话框（新建 folder / 重命名用）
function PromptDialog({
  title,
  initial,
  onOk,
  onCancel,
}: {
  title: string;
  initial: string;
  onOk: (v: string) => void;
  onCancel: () => void;
}) {
  const [v, setV] = useState(initial);
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  return (
    <div className="modal-mask" onMouseDown={onCancel}>
      <div className="modal" onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-title">{title}</div>
        <input
          ref={ref}
          className="modal-input"
          value={v}
          onChange={(e) => setV(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onOk(v);
            else if (e.key === "Escape") onCancel();
          }}
        />
        <div className="modal-actions">
          <button className="btn-ghost" onClick={onCancel}>
            取消
          </button>
          <button className="btn-primary" onClick={() => onOk(v)}>
            确定
          </button>
        </div>
      </div>
    </div>
  );
}

// 可视化新建 case 对话框（名称 + method + URL）
function NewCaseDialog({
  onOk,
  onCancel,
}: {
  onOk: (v: { name: string; method: string; url: string }) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("新用例");
  const [method, setMethod] = useState("GET");
  const [url, setUrl] = useState("https://api.example.com");
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const submit = () => {
    if (name.trim()) onOk({ name, method, url });
  };
  return (
    <div className="modal-mask" onMouseDown={onCancel}>
      <div className="modal modal-wide" onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-title">新建用例</div>
        <div className="field-row">
          <label>名称</label>
          <input
            ref={ref}
            value={name}
            placeholder="用例名（.yml 可省略）"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
              else if (e.key === "Escape") onCancel();
            }}
          />
        </div>
        <div className="field-row">
          <label>请求</label>
          <Select
            className={`nc-method ${methodClass(method)}`}
            value={method}
            options={METHODS.map((m) => ({ value: m, label: m }))}
            onChange={(v) => setMethod(v)}
            ariaLabel="请求方法"
          />
          <input
            className="nc-url"
            value={url}
            placeholder="https://api.example.com/path"
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
              else if (e.key === "Escape") onCancel();
            }}
          />
        </div>
        <div className="modal-actions">
          <button className="btn-ghost" onClick={onCancel}>
            取消
          </button>
          <button className="btn-primary" onClick={submit}>
            创建
          </button>
        </div>
      </div>
    </div>
  );
}

// application.yml 的可视化设置页：左导航 + 右配置面板（仿 GitHub 设置页）
// 配置页「快捷键」分区：查看 + 录制重绑 + 冲突检测 + 恢复默认。
function ShortcutsSettings({ overrides, onChange }: { overrides: Overrides; onChange: (next: Overrides) => void }) {
  const [recording, setRecording] = useState<ActionId | null>(null);
  const bindings = resolveBindings(overrides);

  // 录制态：capture 阶段全局捕获 + stopPropagation，避免录制时触发全局快捷键分发
  useEffect(() => {
    const rec = recording;
    if (!rec) return;
    function onKey(e: KeyboardEvent) {
      if (!rec) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setRecording(null);
        return;
      }
      if (e.key === "Backspace" || e.key === "Delete") {
        onChange({ ...overrides, [rec]: "" }); // 清空 → 禁用
        setRecording(null);
        return;
      }
      const accel = eventToAccel(e);
      if (!accel) return; // 仅按了修饰键，等待主键
      const conflict = findConflict(resolveBindings(overrides), accel, rec);
      if (conflict) {
        const ok = window.confirm(`${formatAccel(accel)} 已被「${ACTION_MAP[conflict].label}」占用，替换？（原动作将被解绑）`);
        if (ok) onChange({ ...overrides, [conflict]: "", [rec]: accelKey(accel) });
      } else {
        onChange({ ...overrides, [rec]: accelKey(accel) });
      }
      setRecording(null);
    }
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [recording, overrides, onChange]);

  const groups: { name: string; items: ActionDef[] }[] = [];
  for (const a of ACTIONS) {
    let g = groups.find((x) => x.name === a.group);
    if (!g) {
      g = { name: a.group, items: [] };
      groups.push(g);
    }
    g.items.push(a);
  }

  function restore(id: ActionId) {
    const n = { ...overrides };
    delete n[id];
    onChange(n);
  }

  return (
    <div className="settings-section">
      <div className="settings-title sc-title-row">
        <span>快捷键</span>
        <button className="link-danger" onClick={() => onChange({})}>
          全部恢复默认
        </button>
      </div>
      <div className="settings-desc">
        点「修改」后按下新组合键即可重绑；录制时 <code>Esc</code> 取消、<code>Backspace</code> 清空（禁用）。修饰键{" "}
        <code>Mod</code> 在 macOS 为 ⌘、其它平台为 Ctrl。
      </div>
      {groups.map((g) => (
        <div key={g.name} className="sc-group">
          <div className="sc-group-name">{g.name}</div>
          {g.items.map((a) => {
            const accel = bindings[a.id];
            const isRec = recording === a.id;
            return (
              <div key={a.id} className="sc-row">
                <span className="sc-label">{a.label}</span>
                <span className="sc-spacer" />
                {isRec ? (
                  <span className="sc-badge recording">按下快捷键…</span>
                ) : accel ? (
                  <span className="sc-keys">
                    {accelTokens(accel).map((t, i) => (
                      <kbd key={i} className="sc-key">
                        {t}
                      </kbd>
                    ))}
                  </span>
                ) : (
                  <span className="sc-badge disabled">已禁用</span>
                )}
                <button className="sc-btn" onClick={() => setRecording(isRec ? null : a.id)}>
                  {isRec ? "取消" : "修改"}
                </button>
                {!isDefaultBinding(overrides, a.id) && (
                  <button className="sc-btn ghost" onClick={() => restore(a.id)}>
                    恢复
                  </button>
                )}
              </div>
            );
          })}
        </div>
      ))}
    </div>
  );
}

function SettingsPage({
  environments,
  onChange,
  workspacePath,
  configPath,
  shortcutOverrides,
  onShortcutChange,
}: {
  environments: Record<string, Record<string, string>>;
  onChange: (next: Record<string, Record<string, string>>) => void;
  workspacePath: string;
  configPath: string;
  shortcutOverrides: Overrides;
  onShortcutChange: (next: Overrides) => void;
}) {
  const NAV = ["通用", "主题", "环境", "快捷键"] as const;
  const [section, setSection] = useState<(typeof NAV)[number]>("环境");
  const envNames = Object.keys(environments);
  const [selEnv, setSelEnv] = useState(envNames[0] || "");
  const cur = envNames.includes(selEnv) ? selEnv : envNames[0] || "";

  function setVars(env: string, rows: { name: string; value: string; enabled?: boolean }[]) {
    const m: Record<string, string> = {};
    for (const r of rows) if (r.name.trim()) m[r.name.trim()] = r.value;
    onChange({ ...environments, [env]: m });
  }
  function addEnv() {
    const name = window.prompt("新环境名称（如 dev / test / prod）", "");
    if (!name || !name.trim()) return;
    const n = name.trim();
    if (environments[n]) {
      window.alert("环境已存在");
      return;
    }
    onChange({ ...environments, [n]: {} });
    setSelEnv(n);
  }
  function delEnv(env: string) {
    if (!window.confirm(`删除环境「${env}」？`)) return;
    const next = { ...environments };
    delete next[env];
    onChange(next);
    setSelEnv(Object.keys(next)[0] || "");
  }

  return (
    <div className="settings">
      <nav className="settings-nav">
        {NAV.map((s) => (
          <button key={s} className={`settings-nav-item ${section === s ? "active" : ""}`} onClick={() => setSection(s)}>
            {s}
          </button>
        ))}
      </nav>
      <div className="settings-panel">
        {section === "环境" && (
          <div className="settings-section">
            <div className="settings-title">环境</div>
            <div className="settings-desc">
              多套环境，运行时用 <code>{"{{变量名}}"}</code> 引用；右上角可切换活动环境。case 级 vars 会覆盖同名环境变量。
            </div>
            <div className="env-tabs">
              {envNames.map((e) => (
                <button key={e} className={`env-tab ${e === cur ? "active" : ""}`} onClick={() => setSelEnv(e)}>
                  {e}
                </button>
              ))}
              <button className="env-tab add" onClick={addEnv}>
                ＋ 添加环境
              </button>
            </div>
            {cur ? (
              <>
                <div className="env-head">
                  <span className="env-cur-name">{cur}</span>
                  <button className="link-danger" onClick={() => delEnv(cur)}>
                    删除此环境
                  </button>
                </div>
                <KVTable
                  rows={Object.entries(environments[cur] || {}).map(([name, value]) => ({ name, value, enabled: true }))}
                  onChange={(rows) => setVars(cur, rows)}
                  namePlaceholder="变量名"
                  valuePlaceholder="值（如 https://api.demo.com）"
                />
              </>
            ) : (
              <div className="settings-empty">暂无环境，点「添加环境」新建。</div>
            )}
          </div>
        )}
        {section === "通用" && (
          <div className="settings-section">
            <div className="settings-title">通用</div>
            <div className="settings-desc">工作空间基本信息（只读）。</div>
            <div className="field-row">
              <label>工作空间</label>
              <input readOnly value={workspacePath} />
            </div>
            <div className="field-row">
              <label>配置文件</label>
              <input readOnly value={configPath} />
            </div>
          </div>
        )}
        {section === "主题" && (
          <div className="settings-section">
            <div className="settings-title">主题</div>
            <div className="settings-desc">当前为浅色主题；深色主题即将支持。</div>
          </div>
        )}
        {section === "快捷键" && <ShortcutsSettings overrides={shortcutOverrides} onChange={onShortcutChange} />}
      </div>
    </div>
  );
}

// 标签页栏（多文件打开）：中键 / × 关闭，右键弹关闭菜单
function TabBar({
  tabs,
  active,
  isDirty,
  onSelect,
  onClose,
  onContext,
}: {
  tabs: string[];
  active: string;
  isDirty: (path: string) => boolean;
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
  onContext: (e: React.MouseEvent, path: string) => void;
}) {
  return (
    <div className="tab-bar">
      {tabs.map((path) => (
        <div
          key={path}
          className={`file-tab ${path === active ? "active" : ""}`}
          title={path}
          onMouseDown={(e) => {
            if (e.button === 1) {
              e.preventDefault();
              onClose(path);
            }
          }}
          onClick={() => onSelect(path)}
          onContextMenu={(e) => onContext(e, path)}
        >
          <span className="ft-name">{baseName(path)}</span>
          <span className="ft-right">
            {isDirty(path) && <span className="ft-dirty" />}
            <button
              className="ft-close"
              title="关闭"
              onClick={(e) => {
                e.stopPropagation();
                onClose(path);
              }}
            >
              ×
            </button>
          </span>
        </div>
      ))}
    </div>
  );
}

// 三栏布局显隐标志（左文件树 / 底部终端 / 右侧 AI）。
// 用 sessionStorage 而非 localStorage：只在本次运行内记忆（含 dev 热重载/刷新），
// 应用整体关闭再启动即视为全新会话，回退到默认「三栏全关」。
interface LayoutFlags {
  left: boolean;
  bottom: boolean;
  right: boolean;
}
const LAYOUT_KEY = "apicase.layout.v1";
function loadLayout(): LayoutFlags {
  const fallback: LayoutFlags = { left: false, bottom: false, right: false };
  try {
    const raw = sessionStorage.getItem(LAYOUT_KEY);
    if (!raw) return fallback;
    const o = JSON.parse(raw);
    return {
      left: typeof o.left === "boolean" ? o.left : fallback.left,
      bottom: typeof o.bottom === "boolean" ? o.bottom : fallback.bottom,
      right: typeof o.right === "boolean" ? o.right : fallback.right,
    };
  } catch {
    return fallback;
  }
}

function App() {
  // 工作空间
  const [workspace, setWorkspace] = useState("");
  const [recentWorkspaces, setRecentWorkspaces] = useState<string[]>([]);
  const [wsMenuOpen, setWsMenuOpen] = useState(false);
  const wsMenuRef = useRef<HTMLDivElement>(null);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(200);
  const resizingRef = useRef(false);
  // 三栏布局显隐（顶栏切换）：左=文件树 / 底=终端 / 右=AI 对话；仅本次运行内记忆
  const [layout, setLayout] = useState<LayoutFlags>(() => loadLayout());
  const { left: showLeft, bottom: showBottom, right: showRight } = layout;
  const toggleBottom = () => setLayout((l) => ({ ...l, bottom: !l.bottom }));
  const toggleRight = () => setLayout((l) => ({ ...l, right: !l.right }));
  // 左栏可独立开关：无工作空间时开关照常可用，仅内容显示为空态提示（引导打开工作空间）
  const effectiveShowLeft = showLeft;
  const toggleLeft = () => setLayout((l) => ({ ...l, left: !l.left }));
  // 底部终端一旦打开即常驻（隐藏而非卸载），保持 shell 会话与滚动；右侧 AI 同理
  const termEverOpened = useRef(false);
  if (showBottom) termEverOpened.current = true;
  const aiEverOpened = useRef(false);
  if (showRight) aiEverOpened.current = true;
  // 底部终端高度（px，可拖）+ 右侧 AI 宽度（px，可拖）
  const [bottomHeight, setBottomHeight] = useState(240);
  const bottomResizingRef = useRef(false);
  // 多终端（仿 VSCode/Postman）：底部栏可开多个 shell，右侧列表切换/关闭。
  // cwd 在创建时快照——切换工作空间不影响已开终端；新开的终端用当前工作空间。
  const [terminals, setTerminals] = useState<{ id: string; cwd: string; n: number }[]>([]);
  const [activeTermId, setActiveTermId] = useState("");
  const termSeqRef = useRef(0);
  function addTerminal() {
    const n = termSeqRef.current + 1;
    termSeqRef.current = n;
    const t = { id: `bterm-${n}`, cwd: workspace, n };
    setTerminals((prev) => [...prev, t]);
    setActiveTermId(t.id);
  }
  function closeTerminal(id: string) {
    const idx = terminals.findIndex((t) => t.id === id);
    if (idx < 0) return;
    const next = terminals.filter((t) => t.id !== id);
    setTerminals(next);
    if (activeTermId === id) {
      const neighbor = next[idx] || next[idx - 1];
      setActiveTermId(neighbor ? neighbor.id : "");
    }
    if (next.length === 0) setLayout((l) => ({ ...l, bottom: false })); // 关掉最后一个即收起底部栏
  }
  // 底部栏打开且尚无终端时，自动创建一个（首次开栏 / 关净后重开）
  useEffect(() => {
    if (showBottom && terminals.length === 0) addTerminal();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showBottom]);
  const [aiWidth, setAiWidth] = useState(320);
  const aiResizingRef = useRef(false);
  const centerColRef = useRef<HTMLDivElement>(null);
  // 流程/请求分栏：流程面板宽度（px）。null → 用 CSS 默认 44%；拖动后固定为像素值
  const [flowPaneWidth, setFlowPaneWidth] = useState<number | null>(null);
  const flowResizingRef = useRef(false);
  const structuredRef = useRef<HTMLDivElement>(null);
  // environment（多套环境）：从工作空间根 application.yml 读取
  const [environments, setEnvironments] = useState<Record<string, Record<string, string>>>({});
  const [activeEnv, setActiveEnv] = useState("");
  const [envMenuOpen, setEnvMenuOpen] = useState(false);
  const envMenuRef = useRef<HTMLDivElement>(null);
  // 文件树
  const [childrenMap, setChildrenMap] = useState<Record<string, DirEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  // 文件系统监听：外部增删改的自愈刷新
  // 本应用刚写过的路径 → 时间戳，用于抑制监听回声（避免自身保存触发重载/覆盖）
  const selfWritesRef = useRef<Map<string, number>>(new Map());
  // 监听器每次触发时读取的最新处理闭包（避免一次性订阅捕获过期 state）
  const fsHandlerRef = useRef<(paths: string[]) => void>(() => {});
  // 活动文件被外部修改且存在未保存改动时的提示（不静默覆盖用户编辑）
  const [externalStale, setExternalStale] = useState(false);
  // 文件树/搜索的选中高亮直接以 currentCasePath 为准（当前打开的文件），无需单独状态
  // 搜索栏 / 可视化新建
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<DirEntry[]>([]);
  const [newCaseDir, setNewCaseDir] = useState<string | null>(null);
  // 右键菜单 / 输入对话框
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; entry: DirEntry | null } | null>(null);
  const [promptState, setPromptState] = useState<{ title: string; initial: string; onOk: (v: string) => void } | null>(null);

  // 多标签页：打开顺序 + 非活动标签的状态快照（活动标签用下方 live state）
  const [tabOrder, setTabOrder] = useState<string[]>([]);
  const tabCacheRef = useRef<Record<string, TabSnapshot>>({});
  const [tabMenu, setTabMenu] = useState<{ x: number; y: number; path: string } | null>(null);

  // 当前打开的 case（活动标签）
  const [currentCasePath, setCurrentCasePath] = useState("");
  const [caseName, setCaseName] = useState("");
  const [caseVars, setCaseVars] = useState<Record<string, unknown> | undefined>(undefined);
  const [caseVersion, setCaseVersion] = useState("0.1");
  const [dirty, setDirty] = useState(false);

  // 统一 requests 模型（单请求 = 长度 1）
  const [requests, setRequests] = useState<RequestDraft[]>([]);
  const [selectedRequestId, setSelectedRequestId] = useState("");
  const [uiNodes, setUiNodes] = useState<UiNodes | undefined>(undefined);

  // 视图切换：文本互斥；流程 / 请求为结构化分栏
  const [textMode, setTextMode] = useState(false);
  const [showFlow, setShowFlow] = useState(false);
  const [showRequest, setShowRequest] = useState(true);
  const [rawText, setRawText] = useState("");
  const [caseValid, setCaseValid] = useState(false);
  const [textError, setTextError] = useState<string | null>(null);
  const [binaryFile, setBinaryFile] = useState(false); // 二进制/不支持编码：显示占位提示
  const [configVisual, setConfigVisual] = useState(false); // application.yml：可视设置页 vs 文本

  // 运行态：每个请求一份（响应区展示当前选中请求）
  const [runMap, setRunMap] = useState<Record<string, RunState>>({});
  const [outputsCtx, setOutputsCtx] = useState<Record<string, Record<string, unknown>>>({});
  const [runningAll, setRunningAll] = useState(false);
  const [respTab, setRespTab] = useState<"body" | "headers">("body");
  const [pretty, setPretty] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // 全局快捷键 override（app 级偏好，持久化 localStorage）
  const [scOverrides, setScOverrides] = useState<Overrides>(() => loadOverrides());
  function onShortcutChange(next: Overrides) {
    setScOverrides(next);
    saveOverrides(next);
  }

  const mark = () => setDirty(true);

  const selected = requests.find((s) => s.id === selectedRequestId) || requests[0];
  const isFlow = requests.length >= 2 || requests.some((s) => s.outputs.length > 0 || s.dependsOn.length > 0);
  const effectiveText = !!currentCasePath && (textMode || requests.length === 0 || (!showFlow && !showRequest));
  // 仅 .yml/.yaml（非 application.yml）可作为 case：决定是否显示流程/请求视图切换
  const caseEligible = !!currentCasePath && isYamlFile(currentCasePath) && !isAppConfig(currentCasePath);
  const isConfig = !!currentCasePath && isAppConfig(currentCasePath);

  // 点击菜单外部时关闭工作空间下拉
  useEffect(() => {
    if (!wsMenuOpen) return;
    function onDocClick(e: MouseEvent) {
      if (wsMenuRef.current && !wsMenuRef.current.contains(e.target as Node)) {
        setWsMenuOpen(false);
      }
    }
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [wsMenuOpen]);

  // 点击外部关闭环境下拉
  useEffect(() => {
    if (!envMenuOpen) return;
    function onDocClick(e: MouseEvent) {
      if (envMenuRef.current && !envMenuRef.current.contains(e.target as Node)) setEnvMenuOpen(false);
    }
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [envMenuOpen]);

  // 三栏布局显隐持久化（仅本次运行内，见 loadLayout 说明）
  useEffect(() => {
    try {
      sessionStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
    } catch {
      /* ignore */
    }
  }, [layout]);

  // 左侧栏拖动调宽
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!resizingRef.current) return;
      setSidebarWidth(Math.min(480, Math.max(160, e.clientX)));
    }
    function onUp() {
      if (!resizingRef.current) return;
      resizingRef.current = false;
      document.body.classList.remove("resizing-col", "resizing-sidebar");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 底部终端栏拖动调高（向上拖增高）
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!bottomResizingRef.current) return;
      const box = centerColRef.current?.getBoundingClientRect();
      if (!box) return;
      // 从中间列底边反推高度；上限留 120px 给主区，下限 80px
      const h = box.bottom - e.clientY;
      setBottomHeight(Math.max(80, Math.min(box.height - 120, h)));
    }
    function onUp() {
      if (!bottomResizingRef.current) return;
      bottomResizingRef.current = false;
      document.body.classList.remove("resizing-row");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 右侧 AI 栏拖动调宽（向左拖增宽）
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!aiResizingRef.current) return;
      const w = window.innerWidth - e.clientX;
      setAiWidth(Math.max(240, Math.min(560, w)));
    }
    function onUp() {
      if (!aiResizingRef.current) return;
      aiResizingRef.current = false;
      document.body.classList.remove("resizing-col", "resizing-ai");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 流程/请求分栏的拖动分割条：调整流程面板宽度（请求面板占剩余空间）
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!flowResizingRef.current) return;
      const box = structuredRef.current?.getBoundingClientRect();
      if (!box) return;
      // 流程面板不小于 260，且尽量给请求面板留 360（260 下限优先）
      const w = Math.max(260, Math.min(box.width - 360, e.clientX - box.left));
      setFlowPaneWidth(w);
    }
    function onUp() {
      if (!flowResizingRef.current) return;
      flowResizingRef.current = false;
      document.body.classList.remove("resizing-col", "resizing-pane");
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, []);

  // 全屏切换（用窗口尺寸变化方向判断，避免退出时 isFullscreen() 滞后）
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let lastW = 0;
    let lastH = 0;
    try {
      const appWindow = getCurrentWindow();
      appWindow.isFullscreen().then(setIsFullscreen).catch(() => {});
      appWindow
        .innerSize()
        .then((s) => {
          lastW = s.width;
          lastH = s.height;
        })
        .catch(() => {});
      appWindow
        .onResized(async ({ payload }) => {
          const { width, height } = payload;
          const shrinking = width < lastW || height < lastH;
          lastW = width;
          lastH = height;
          if (shrinking) {
            setIsFullscreen(false);
            return;
          }
          try {
            setIsFullscreen(await appWindow.isFullscreen());
          } catch {
            // 忽略
          }
        })
        .then((u) => {
          unlisten = u;
        })
        .catch(() => {});
    } catch {
      // 非 Tauri 环境忽略
    }
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // 挂载一次：订阅后端文件系统变更事件，交给最新的处理闭包（fsHandlerRef）
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<string[]>("workspace:fs-change", (e) => {
      fsHandlerRef.current(e.payload || []);
    })
      .then((u) => {
        unlisten = u;
      })
      .catch(() => {});
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // 全局快捷键：单一 document 监听；用 ref 取最新绑定 / 动作闭包
  const saveRef = useRef<() => void>(() => {});
  const scLookupRef = useRef<Record<string, ActionId>>({});
  const scActionsRef = useRef<Partial<Record<ActionId, () => void>>>({});
  const searchInputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const accel = eventToAccel(e);
      if (!accel) return;
      // 纯键（无 Mod/Alt）在输入类元素中不拦截，避免干扰打字
      if (!accel.mod && !accel.alt) {
        const t = e.target as HTMLElement | null;
        const tag = t?.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || t?.isContentEditable) return;
      }
      const id = scLookupRef.current[accelKey(accel)];
      if (!id) return;
      e.preventDefault();
      scActionsRef.current[id]?.();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  // 搜索：有词时后端递归扫描（debounce 200ms）；清空即恢复文件树
  useEffect(() => {
    if (!workspace) return;
    const q = searchQuery.trim();
    if (q === "") {
      setSearchResults([]);
      return;
    }
    const t = setTimeout(() => {
      invoke<DirEntry[]>("search_workspace", { root: workspace, query: q })
        .then(setSearchResults)
        .catch(() => {});
    }, 200);
    return () => clearTimeout(t);
  }, [searchQuery, workspace]);

  // 读取某目录的直接子项并缓存
  async function loadDir(path: string) {
    try {
      const entries = await invoke<DirEntry[]>("list_dir", { path });
      setChildrenMap((prev) => ({ ...prev, [path]: entries }));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  // 记录本应用自身发起的写操作路径，令监听回声可被识别并抑制
  function noteSelfWrite(...paths: string[]) {
    const now = Date.now();
    paths.forEach((p) => selfWritesRef.current.set(p, now));
  }

  function toggleDir(entry: DirEntry) {
    const isOpen = expanded.has(entry.path);
    setExpanded((prev) => {
      const next = new Set(prev);
      if (isOpen) next.delete(entry.path);
      else next.add(entry.path);
      return next;
    });
    if (!isOpen && !childrenMap[entry.path]) {
      loadDir(entry.path);
    }
  }

  // 在文件树中「显露」某个文件：展开它与工作空间根之间的各级父目录，并懒加载其子项，
  // 使折叠目录里的文件（如从 Tab 切过去）能正确展开并高亮可见。
  function revealInTree(path: string) {
    if (!workspace || !path.startsWith(workspace)) return;
    const ancestors: string[] = [];
    let d = dirName(path);
    // 收集根与文件之间的所有中间目录（不含根，根在树中始终显示）
    while (d.length > workspace.length && d.startsWith(workspace)) {
      ancestors.push(d);
      const parent = dirName(d);
      if (parent === d) break; // 防御：路径已到顶，dirName 不再变化
      d = parent;
    }
    if (ancestors.length === 0) return; // 文件就在根目录下，无需展开
    setExpanded((prev) => {
      const next = new Set(prev);
      ancestors.forEach((a) => next.add(a));
      return next;
    });
    // 未加载 children 的目录先加载，否则展开后子树为空、文件仍不可见
    ancestors.forEach((a) => {
      if (!childrenMap[a]) loadDir(a);
    });
  }

  // 活动文件变化（点 Tab、关标签切邻居、新建等）时，自动在文件树中展开显露它
  useEffect(() => {
    revealInTree(currentCasePath);
    setExternalStale(false); // 切换活动文件即清除上一个文件的外部改动提示
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentCasePath, workspace]);

  function applyWorkspace(path: string) {
    setWorkspace(path);
    setRecentWorkspaces((prev) => [path, ...prev.filter((p) => p !== path)].slice(0, 10));
    setChildrenMap({});
    setExpanded(new Set());
    closeAllTabsAndReset();
    loadDir(path);
    loadEnvironments(path);
    // 打开 / 切换工作空间即显示左侧文件树（顶栏开关随之点亮）
    setLayout((l) => ({ ...l, left: true }));
    // 启动/切换文件系统监听：外部对该工作空间的增删改将实时回传
    invoke("watch_workspace", { path }).catch(() => {});
  }

  // 读取工作空间 environment（application.yml）并挑选活动环境
  async function loadEnvironments(root: string) {
    try {
      const text = await invoke<string>("read_text_file", { path: joinPath(root, "application.yml") });
      const envs = parseEnvironments(text);
      setEnvironments(envs);
      const names = Object.keys(envs);
      setActiveEnv(names.includes("default") ? "default" : names[0] || "");
    } catch {
      setEnvironments({});
      setActiveEnv("");
    }
  }

  async function openOrCreateWorkspace() {
    setWsMenuOpen(false);
    try {
      const selectedDir = await open({ directory: true, multiple: false, title: "打开工作空间" });
      if (typeof selectedDir === "string") {
        await invoke("init_workspace", { path: selectedDir });
        applyWorkspace(selectedDir);
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  function selectWorkspace(ws: string) {
    applyWorkspace(ws);
    setWsMenuOpen(false);
  }

  // ── case / 标签页 打开 / 关闭 ─────────────────────
  // 重置活动编辑态（不动标签列表）
  function resetCaseState() {
    setCurrentCasePath("");
    setCaseName("");
    setCaseVars(undefined);
    setRequests([]);
    setSelectedRequestId("");
    setUiNodes(undefined);
    setRawText("");
    setCaseValid(false);
    setTextError(null);
    setBinaryFile(false);
    setConfigVisual(false);
    setTextMode(false);
    setRunMap({});
    setOutputsCtx({});
    setDirty(false);
  }

  function closeAllTabsAndReset() {
    tabCacheRef.current = {};
    setTabOrder([]);
    resetCaseState();
  }

  // 把当前活动标签的完整状态打成快照
  function snapshotCurrent(): TabSnapshot | null {
    if (!currentCasePath) return null;
    return {
      path: currentCasePath,
      caseName,
      caseVars,
      caseVersion,
      dirty,
      requests,
      selectedRequestId,
      uiNodes,
      textMode,
      showFlow,
      showRequest,
      rawText,
      caseValid,
      textError,
      binaryFile,
      configVisual,
      runMap,
      outputsCtx,
      respTab,
      pretty,
      error,
    };
  }

  function restoreSnapshot(s: TabSnapshot) {
    setCurrentCasePath(s.path);
    setCaseName(s.caseName);
    setCaseVars(s.caseVars);
    setCaseVersion(s.caseVersion);
    setDirty(s.dirty);
    setRequests(s.requests);
    setSelectedRequestId(s.selectedRequestId);
    setUiNodes(s.uiNodes);
    setTextMode(s.textMode);
    setShowFlow(s.showFlow);
    setShowRequest(s.showRequest);
    setRawText(s.rawText);
    setCaseValid(s.caseValid);
    setTextError(s.textError);
    setBinaryFile(s.binaryFile);
    setConfigVisual(s.configVisual);
    setRunMap(s.runMap);
    setOutputsCtx(s.outputsCtx);
    setRespTab(s.respTab);
    setPretty(s.pretty);
    setError(s.error);
  }

  const isDirtyPath = (p: string): boolean => (p === currentCasePath ? dirty : tabCacheRef.current[p]?.dirty ?? false);

  // 打开一个标签（新开则从磁盘加载，已开则恢复其内存状态）
  function openTab(path: string) {
    if (path === currentCasePath) return;
    const snap = snapshotCurrent();
    if (snap) tabCacheRef.current[snap.path] = snap;
    if (tabOrder.includes(path) && tabCacheRef.current[path]) {
      restoreSnapshot(tabCacheRef.current[path]);
    } else {
      openCase(path); // 读取成功后再入标签，避免二进制读取失败留下空标签
    }
  }

  function closeTab(path: string) {
    if (isDirtyPath(path) && !window.confirm(`「${baseName(path)}」有未保存修改，仍关闭？`)) return;
    const wasActive = path === currentCasePath;
    const idx = tabOrder.indexOf(path);
    const rest = tabOrder.filter((p) => p !== path);
    delete tabCacheRef.current[path];
    setTabOrder(rest);
    if (wasActive) {
      if (rest.length === 0) {
        resetCaseState();
      } else {
        const neighbor = rest[Math.min(idx, rest.length - 1)];
        const s = tabCacheRef.current[neighbor];
        if (s) restoreSnapshot(s);
        else openCase(neighbor);
      }
    }
  }

  // 外部删除：静默关闭指向该路径的标签并切到邻居（文件已不存在，无需确认）
  function dropOpenTab(path: string) {
    const idx = tabOrder.indexOf(path);
    if (idx === -1) return;
    const wasActive = path === currentCasePath;
    const rest = tabOrder.filter((p) => p !== path);
    delete tabCacheRef.current[path];
    setTabOrder(rest);
    if (wasActive) {
      setExternalStale(false);
      if (rest.length === 0) {
        resetCaseState();
      } else {
        const neighbor = rest[Math.min(idx, rest.length - 1)];
        const s = tabCacheRef.current[neighbor];
        if (s) restoreSnapshot(s);
        else openCase(neighbor);
      }
    }
  }

  function closeOtherTabs(keep: string) {
    const others = tabOrder.filter((p) => p !== keep);
    if (others.some(isDirtyPath) && !window.confirm("其它标签页有未保存修改，仍关闭？")) return;
    if (currentCasePath !== keep) {
      const s = tabCacheRef.current[keep];
      if (s) restoreSnapshot(s);
      else openCase(keep);
    }
    others.forEach((p) => delete tabCacheRef.current[p]);
    delete tabCacheRef.current[keep];
    setTabOrder([keep]);
  }

  function closeAllTabs() {
    if (tabOrder.some(isDirtyPath) && !window.confirm("有未保存的标签页，仍全部关闭？")) return;
    closeAllTabsAndReset();
  }

  // 把一个已解析 Case 应用到结构化编辑态（保持已选请求）
  function applyCase(c: Case) {
    const { requests: rd, ui } = caseToRequests(c);
    setRequests(rd);
    setUiNodes(ui);
    setCaseName(c.name || "");
    setCaseVars(c.vars);
    setCaseVersion(c.version || "0.1");
    setSelectedRequestId((prev) => (rd.some((s) => s.id === prev) ? prev : rd[0].id));
    setCaseValid(true);
  }

  function onSelectFile(path: string) {
    openTab(path); // 任意文件都打开：case 渲染结构、其余落文本、二进制读取失败给提示
  }

  // 打开一个二进制/不支持编码的文件（像 VSCode 一样开标签 + 占位提示，不渲染编辑器）
  function openBinaryTab(path: string) {
    setTabOrder((prev) => (prev.includes(path) ? prev : [...prev, path]));
    setCurrentCasePath(path);
    setBinaryFile(true);
    setConfigVisual(false);
    setCaseName("");
    setCaseVars(undefined);
    setRequests([]);
    setSelectedRequestId("");
    setUiNodes(undefined);
    setRawText("");
    setCaseValid(false);
    setTextError(null);
    setTextMode(false);
    setRunMap({});
    setOutputsCtx({});
    setDirty(false);
    setError(null);
  }

  async function openCase(path: string) {
    // 已知二进制/媒体扩展名：直接占位，连 invoke 都省
    if (isBinaryExt(path)) {
      openBinaryTab(path);
      return;
    }
    try {
      // 后端判定文本/二进制（NUL 嗅探 + UTF-8 校验），不再靠错误串匹配
      const fc = await invoke<{ binary: boolean; text: string | null }>("read_file_smart", { path });
      if (fc.binary || fc.text === null) {
        openBinaryTab(path);
        return;
      }
      const text = fc.text;
      setTabOrder((prev) => (prev.includes(path) ? prev : [...prev, path]));
      setBinaryFile(false);
      setCurrentCasePath(path);
      setDirty(false);
      setError(null);
      setRunMap({});
      setOutputsCtx({});
      setRawText(text);
      // application.yml：默认进可视设置页，并按文件内容同步环境
      setConfigVisual(isAppConfig(path));
      if (isAppConfig(path)) setEnvironments(parseEnvironments(text));
      // 仅 .yml/.yaml（非 application.yml）才按 case 解析渲染；其余一律纯文本——
      // 避免把恰好符合格式的 .txt/.json 误渲染成结构化编辑器（保存会用 YAML 覆盖、丢内容）
      const canBeCase = isYamlFile(path) && !isAppConfig(path);
      const res = canBeCase ? analyzeCase(text) : null;
      if (!res || !res.valid || !res.case) {
        // 非 case 或校验不通过 → 纯文本兜底（非 .yml 文件不挂"不是有效用例"提示）
        setRequests([]);
        setSelectedRequestId("");
        setUiNodes(undefined);
        setCaseValid(false);
        setTextError(res ? res.error || "不是有效的用例" : null);
        setTextMode(true);
        setShowFlow(false);
        setShowRequest(true);
      } else {
        applyCase(res.case);
        setTextError(null);
        setTextMode(false);
        // 内容驱动默认视图：多请求 → 流程+请求；单请求 → 请求
        const list = res.case.requests;
        const multi = list.length >= 2 || list.some((s) => s.outputs.length || s.dependsOn.length);
        setShowFlow(multi);
        setShowRequest(true);
      }
    } catch (e) {
      // 到这里都是真实 IO 错误（找不到/无权限）；二进制判定已在后端完成
      setError(typeof e === "string" ? e : String(e));
    }
  }

  // ── 内部状态 → Case（保存 / 文本 dump 的公共路径）──
  function stateToCase(): { case?: Case; error?: string } {
    const out: Request[] = [];
    for (const rd of requests) {
      const { request, error: err } = draftToRequest(rd.req);
      if (err || !request) return { error: `请求「${rd.id}」：${err || "请求非法"}` };
      out.push({ id: rd.id, http: request, dependsOn: rd.dependsOn, outputs: rd.outputs, assertions: rd.assertions });
    }
    if (out.length === 0) return { error: "无请求" };
    const c: Case = {
      version: caseVersion || "0.1",
      name: caseName || undefined,
      vars: caseVars,
      requests: out,
    };
    if (uiNodes && Object.keys(uiNodes).length) c.ui = { nodes: uiNodes };
    return { case: c };
  }

  function currentDump(): { text?: string; error?: string } {
    const { case: c, error: err } = stateToCase();
    if (err || !c) return { error: err };
    return { text: dumpCase(c) };
  }

  // ── 视图切换 ────────────────────────────────────
  function enterText() {
    // 未修改：保留原始文件文本（含注释/格式，忠实展示）；
    // 有结构化改动：从结构态重新 dump 以反映编辑（注释不可避免地丢失）。
    if (dirty) {
      const { text, error: err } = currentDump();
      if (!err && text !== undefined) setRawText(text);
      else if (err) setError(err);
    }
    setTextMode(true);
  }

  function commitText(): Case | null {
    const res = analyzeCase(rawText);
    if (!res.valid || !res.case) {
      window.alert(`YAML 无效，无法切换到结构视图：\n${res.error || "未知错误"}`);
      return null;
    }
    applyCase(res.case);
    setTextError(null);
    return res.case;
  }

  const onClickText = () => enterText();

  // 流程/请求切换：关掉当前唯一在显的面板 → 切到文本
  function onClickFlow() {
    if (showFlow && !showRequest) {
      setShowFlow(false);
      enterText();
      return;
    }
    setShowFlow((v) => !v);
  }

  function onClickRequest() {
    if (showRequest && !showFlow) {
      setShowRequest(false);
      enterText();
      return;
    }
    setShowRequest((v) => !v);
  }

  // 用例：点「可视」进结构视图（文本先提交回结构）；两面板都关时按内容驱动默认
  function onClickVisual() {
    if (!effectiveText) return;
    let multi = isFlow;
    if (textMode) {
      const c = commitText();
      if (!c) return;
      // 用刚解析的 case 判断多请求，避免 setRequests 异步导致 isFlow 滞后
      multi = c.requests.length >= 2 || c.requests.some((s) => s.outputs.length > 0 || s.dependsOn.length > 0);
    }
    setTextMode(false);
    if (!showFlow && !showRequest) {
      // 多请求 → 流程 + 请求；单请求 → 请求
      setShowRequest(true);
      if (multi) setShowFlow(true);
    }
  }

  // application.yml：文本 ↔ 可视设置页
  function enterConfigVisual() {
    if (configVisual) return;
    setEnvironments(parseEnvironments(rawText)); // 以文本为准同步到可视
    setConfigVisual(true);
  }
  function exitConfigVisual() {
    if (!configVisual) return;
    if (dirty) setRawText(dumpApplicationConfig(rawText, environments)); // 有编辑才回写文本（保留原注释除非改过）
    setConfigVisual(false);
  }
  // 可视设置页编辑环境：更新全局 environments + 保持 activeEnv 有效 + 标脏
  function onEnvChange(next: Record<string, Record<string, string>>) {
    setEnvironments(next);
    const names = Object.keys(next);
    if (activeEnv && !names.includes(activeEnv)) setActiveEnv(names.includes("default") ? "default" : names[0] || "");
    mark();
  }

  // ── 保存 ────────────────────────────────────────
  async function saveCase() {
    if (!currentCasePath) return;
    noteSelfWrite(currentCasePath); // 抑制本次保存的监听回声
    try {
      if (isAppConfig(currentCasePath) && configVisual) {
        // 可视设置页：把 environments 序列化进 application.yml
        const content = dumpApplicationConfig(rawText, environments);
        await invoke("write_text_file", { path: currentCasePath, content });
        setRawText(content);
        const names = Object.keys(environments);
        if (!names.includes(activeEnv)) setActiveEnv(names.includes("default") ? "default" : names[0] || "");
      } else if (effectiveText) {
        await invoke("write_text_file", { path: currentCasePath, content: rawText });
        // application.yml：保存后重载 environment 使切换即时生效
        if (isAppConfig(currentCasePath)) {
          const envs = parseEnvironments(rawText);
          setEnvironments(envs);
          const names = Object.keys(envs);
          if (!names.includes(activeEnv)) setActiveEnv(names.includes("default") ? "default" : names[0] || "");
        }
        // 仅 .yml/.yaml：文本此时有效则回填结构态；非 case 文件（.txt/.json）不解析、保持纯文本
        if (isYamlFile(currentCasePath) && !isAppConfig(currentCasePath)) {
          const res = analyzeCase(rawText);
          if (res.valid && res.case) {
            applyCase(res.case);
            setTextError(null);
          } else {
            setCaseValid(false);
            setTextError(res.error || null);
          }
        }
      } else {
        const { text, error: err } = currentDump();
        if (err || text === undefined) {
          setError(err || "序列化失败");
          return;
        }
        await invoke("write_text_file", { path: currentCasePath, content: text });
        setRawText(text);
      }
      setDirty(false);
      setError(null);
      setExternalStale(false);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }
  saveRef.current = () => {
    if (currentCasePath && dirty) saveCase();
  };

  // 快捷键：反查表 + 动作分发闭包（每次 render 取最新 state / handler）
  const scBindings = resolveBindings(scOverrides);
  scLookupRef.current = buildLookup(scBindings);
  scActionsRef.current = {
    "new-case": () => {
      if (workspace) newCaseIn(workspace);
    },
    "open-workspace": () => openOrCreateWorkspace(),
    save: () => saveRef.current(),
    "close-tab": () => {
      if (currentCasePath) closeTab(currentCasePath);
    },
    search: () => {
      searchInputRef.current?.focus();
      searchInputRef.current?.select();
    },
    "open-settings": () => {
      if (!workspace) return;
      openTab(joinPath(workspace, "application.yml"));
      setConfigVisual(true);
    },
    "send-request": () => {
      if (selected) onSendRequest(selected.id);
    },
  };

  // 文件系统变更处理（监听器每次触发时经 fsHandlerRef 读取此最新闭包）
  fsHandlerRef.current = (paths: string[]) => {
    if (!workspace) return;
    const under = (p: string) => p === workspace || p.startsWith(workspace + "/") || p.startsWith(workspace + "\\");
    const inWs = paths.filter(under);
    if (inWs.length === 0) return;

    const now = Date.now();
    const isEcho = (p: string) => {
      const t = selfWritesRef.current.get(p);
      return t !== undefined && now - t < 2500; // 本应用刚写过：忽略回声
    };

    // 1) 目录树：刷新受影响且「已加载」的目录（懒加载一致——不主动展开新目录）
    const dirs = new Set<string>();
    for (const p of inWs) {
      const parent = dirName(p);
      if (parent === workspace || childrenMap[parent] !== undefined) dirs.add(parent); // 增/删/改名改变父目录列表
      if (childrenMap[p] !== undefined) dirs.add(p); // 受影响路径本身是已展开目录
    }
    dirs.forEach((d) => loadDir(d));

    // 2) application.yml 外部改动 → 重载环境（非活动文件时；活动文件走下方重载）
    const cfg = joinPath(workspace, "application.yml");
    if (inWs.includes(cfg) && !isEcho(cfg) && currentCasePath !== cfg) {
      loadEnvironments(workspace);
    }

    // 3) 已打开标签受影响：核对存在性——删除→关标签；活动文件被改→净态重载 / 脏态提示
    const affected = tabOrder.filter((p) => inWs.includes(p) && !isEcho(p));
    affected.forEach((p) => {
      invoke<boolean>("path_exists", { path: p })
        .then((exists) => {
          if (!exists) {
            dropOpenTab(p);
          } else if (p === currentCasePath && !binaryFile) {
            if (dirty) setExternalStale(true); // 有未保存改动：提示，绝不静默覆盖
            else openCase(p); // 净态：直接加载最新内容
          }
        })
        .catch(() => {});
    });
  };

  // ── 请求编辑 ────────────────────────────────────
  function updateReq(next: ReqDraft) {
    setRequests((prev) => prev.map((s) => (s.id === selectedRequestId ? { ...s, req: next } : s)));
    mark();
  }

  function setOutputs(list: RequestOutput[]) {
    setRequests((prev) => prev.map((s) => (s.id === selectedRequestId ? { ...s, outputs: list } : s)));
    mark();
  }

  function setAssertions(list: Assertion[]) {
    setRequests((prev) => prev.map((s) => (s.id === selectedRequestId ? { ...s, assertions: list } : s)));
    mark();
  }

  function renameRequest(oldId: string, newId: string) {
    if (requests.some((s) => s.id === newId)) {
      window.alert("请求 ID 已存在");
      return;
    }
    setRequests((prev) =>
      prev.map((s) => ({
        ...s,
        id: s.id === oldId ? newId : s.id,
        dependsOn: s.dependsOn.map((d) => (d === oldId ? newId : d)),
      })),
    );
    if (selectedRequestId === oldId) setSelectedRequestId(newId);
    setUiNodes((prev) => {
      if (!prev || !prev[oldId]) return prev;
      const next = { ...prev };
      next[newId] = next[oldId];
      delete next[oldId];
      return next;
    });
    setRunMap((prev) => {
      if (!prev[oldId]) return prev;
      const next = { ...prev };
      next[newId] = next[oldId];
      delete next[oldId];
      return next;
    });
    mark();
  }

  function addRequest() {
    const existing = new Set(requests.map((s) => s.id));
    let i = requests.length + 1;
    let id = `req${i}`;
    while (existing.has(id)) {
      i++;
      id = `req${i}`;
    }
    const dependsOn = selectedRequestId ? [selectedRequestId] : [];
    setRequests((prev) => [...prev, { id, dependsOn, outputs: [], assertions: [], req: emptyDraft("GET", "") }]);
    setSelectedRequestId(id);
    setShowFlow(true);
    setShowRequest(true);
    mark();
  }

  function deleteRequest(id: string) {
    if (requests.length <= 1) return;
    const next = requests.filter((s) => s.id !== id).map((s) => ({ ...s, dependsOn: s.dependsOn.filter((d) => d !== id) }));
    setRequests(next);
    if (selectedRequestId === id) setSelectedRequestId(next[0].id);
    setRunMap((prev) => {
      const n = { ...prev };
      delete n[id];
      return n;
    });
    // 清掉被删节点的手动坐标
    setUiNodes((prev) => {
      if (!prev || !prev[id]) return prev;
      const nx = { ...prev };
      delete nx[id];
      return nx;
    });
    mark();
  }

  // 拖动节点：把坐标写入 uiNodes（画布视图态，随 case 一并保存）
  function moveNode(id: string, x: number, y: number) {
    setUiNodes((prev) => ({ ...(prev || {}), [id]: { x, y } }));
    mark();
  }

  // 端口连线建依赖：edge from→to 表示「to 依赖 from」；防自连、防重复、防成环
  function addDependency(fromId: string, toId: string) {
    if (fromId === toId) return;
    const byId = new Map(requests.map((r) => [r.id, r]));
    // from 若已（间接）依赖 to，则再让 to 依赖 from 会成环
    const reaches = (start: string, target: string): boolean => {
      const seen = new Set<string>();
      const stack = [start];
      while (stack.length) {
        const cur = stack.pop() as string;
        if (cur === target) return true;
        if (seen.has(cur)) continue;
        seen.add(cur);
        const n = byId.get(cur);
        if (n) stack.push(...n.dependsOn);
      }
      return false;
    };
    if (reaches(fromId, toId)) {
      window.alert("无法建立依赖：会形成环。");
      return;
    }
    let changed = false;
    setRequests((prev) =>
      prev.map((s) => {
        if (s.id !== toId || s.dependsOn.includes(fromId)) return s;
        changed = true;
        return { ...s, dependsOn: [...s.dependsOn, fromId] };
      }),
    );
    if (changed) mark();
  }

  // 解除依赖：从 toId.dependsOn 移除 fromId
  function removeDependency(fromId: string, toId: string) {
    let changed = false;
    setRequests((prev) =>
      prev.map((s) => {
        if (s.id !== toId || !s.dependsOn.includes(fromId)) return s;
        changed = true;
        return { ...s, dependsOn: s.dependsOn.filter((d) => d !== fromId) };
      }),
    );
    if (changed) mark();
  }

  // 规整：清除全部手动坐标，恢复自动分层布局
  function resetLayout() {
    if (!uiNodes || Object.keys(uiNodes).length === 0) return;
    setUiNodes(undefined);
    mark();
  }

  // ── 运行 ────────────────────────────────────────
  // 单个请求执行：变量透传 → 发送 → 提取 outputs → 评估断言
  async function runRequestWithCtx(sd: RequestDraft, ctx: RunContext): Promise<{ state: RunState; outputs: Record<string, unknown> }> {
    try {
      const resolved = resolveDraft(sd.req, ctx);
      const result = await invoke<ApiResponse>("send_request", { request: buildApiRequest(resolved) });
      const outputs = extractOutputs(sd.outputs, result.body);
      const asserts = evalAssertions(sd.assertions, result);
      const pass = asserts.every((a) => a.ok);
      return { state: { status: pass ? "ok" : "err", resp: result, asserts }, outputs };
    } catch (e) {
      return { state: { status: "err", error: typeof e === "string" ? e : String(e) }, outputs: {} };
    }
  }

  async function onSendRequest(reqId: string) {
    const sd = requests.find((s) => s.id === reqId);
    if (!sd) return;
    if (!sd.req.url.trim()) {
      setRunMap((m) => ({ ...m, [reqId]: { status: "err", error: "请先填写 URL" } }));
      return;
    }
    // 变量优先级：case 级 vars 覆盖 environment（case-local 更具体）
    const ctx: RunContext = { vars: { ...(environments[activeEnv] || {}), ...(caseVars || {}) }, requests: outputsCtx };
    setRunMap((m) => ({ ...m, [reqId]: { status: "running" } }));
    const { state, outputs } = await runRequestWithCtx(sd, ctx);
    setRunMap((m) => ({ ...m, [reqId]: state }));
    setOutputsCtx((prev) => ({ ...prev, [reqId]: outputs }));
    setRespTab("body");
  }

  async function onRunAll() {
    setRunningAll(true);
    // 本地上下文在 await 间同步透传 outputs（不依赖异步 state）
    const local: RunContext = { vars: { ...(environments[activeEnv] || {}), ...(caseVars || {}) }, requests: {} };
    setOutputsCtx({});
    for (const sd of topoOrder(requests)) {
      setRunMap((m) => ({ ...m, [sd.id]: { status: "running" } }));
      const { state, outputs } = await runRequestWithCtx(sd, local);
      local.requests[sd.id] = outputs;
      setOutputsCtx({ ...local.requests });
      setRunMap((m) => ({ ...m, [sd.id]: state }));
    }
    setRunningAll(false);
  }

  // ── 文件管理（右键菜单触发）─────────────────────
  function newCaseIn(dir: string) {
    setNewCaseDir(dir);
  }

  async function createCaseFile(dir: string, name: string, method: string, url: string) {
    let fname = name.trim() || "新用例";
    if (!/\.(yml|yaml)$/i.test(fname)) fname += ".yml";
    const path = joinPath(dir, fname);
    const split = splitQueryFromUrl(url.trim());
    const c: Case = {
      version: "0.1",
      requests: [
        {
          id: "req1",
          http: { method, url: split.base, query: split.query, headers: [], auth: { type: "none" }, body: { type: "none" } },
          dependsOn: [],
          outputs: [],
          assertions: [],
        },
      ],
    };
    try {
      noteSelfWrite(path);
      await invoke("create_file", { path, content: dumpCase(c) });
      await loadDir(dir);
      setExpanded((prev) => new Set(prev).add(dir));
      openTab(path);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  function newFolderIn(dir: string) {
    setPromptState({
      title: "新建文件夹名称",
      initial: "新文件夹",
      onOk: async (name) => {
        if (!name.trim()) return;
        const path = joinPath(dir, name.trim());
        try {
          noteSelfWrite(path);
          await invoke("create_dir", { path });
          await loadDir(dir);
          setExpanded((prev) => new Set(prev).add(dir));
        } catch (e) {
          setError(typeof e === "string" ? e : String(e));
        }
      },
    });
  }

  function renameEntry(entry: DirEntry) {
    setPromptState({
      title: "重命名",
      initial: entry.name,
      onOk: async (name) => {
        if (!name.trim() || name.trim() === entry.name) return;
        const dir = dirName(entry.path);
        const to = joinPath(dir, name.trim());
        try {
          noteSelfWrite(entry.path, to);
          await invoke("rename_path", { from: entry.path, to });
          await loadDir(dir);
          // 同步已打开的标签（精确文件改名）
          if (tabOrder.includes(entry.path)) {
            setTabOrder((prev) => prev.map((p) => (p === entry.path ? to : p)));
            const cached = tabCacheRef.current[entry.path];
            if (cached) {
              tabCacheRef.current[to] = { ...cached, path: to };
              delete tabCacheRef.current[entry.path];
            }
          }
          if (currentCasePath === entry.path) setCurrentCasePath(to);
        } catch (e) {
          setError(typeof e === "string" ? e : String(e));
        }
      },
    });
  }

  async function deleteEntry(entry: DirEntry) {
    if (!window.confirm(`确定删除「${entry.name}」？此操作不可撤销。`)) return;
    const dir = dirName(entry.path);
    try {
      noteSelfWrite(entry.path);
      await invoke("delete_path", { path: entry.path });
      await loadDir(dir);
      // 关闭被删文件/目录下的所有标签
      const under = (p: string) => p === entry.path || p.startsWith(entry.path + "/") || p.startsWith(entry.path + "\\");
      const affected = tabOrder.filter(under);
      if (affected.length) {
        affected.forEach((p) => delete tabCacheRef.current[p]);
        const rest = tabOrder.filter((p) => !under(p));
        setTabOrder(rest);
        if (under(currentCasePath)) {
          if (rest.length) {
            const s = tabCacheRef.current[rest[0]];
            if (s) restoreSnapshot(s);
            else openCase(rest[0]);
          } else {
            resetCaseState();
          }
        }
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  function ctxItems(entry: DirEntry | null) {
    const dir = entry ? (entry.isDir ? entry.path : dirName(entry.path)) : workspace;
    const items: { label: string; onClick: () => void; danger?: boolean }[] = [
      { label: "新建用例", onClick: () => newCaseIn(dir) },
      { label: "新建文件夹", onClick: () => newFolderIn(dir) },
    ];
    if (entry) {
      items.push({ label: "重命名", onClick: () => renameEntry(entry) });
      items.push({ label: "删除", onClick: () => deleteEntry(entry), danger: true });
    }
    return items;
  }

  function openContext(e: React.MouseEvent, entry: DirEntry | null) {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu({ x: e.clientX, y: e.clientY, entry });
  }

  // 画布节点数据
  const flowNodes: FlowNode[] = requests.map((s) => ({
    id: s.id,
    method: s.req.method,
    dependsOn: s.dependsOn,
    status: runMap[s.id]?.status ?? "idle",
  }));

  const run = selected ? runMap[selected.id] : undefined;
  const resp = run?.resp || null;
  const runErr = run?.error || null;
  const sending = run?.status === "running";

  // Tab 行右侧固定控件：视图切换（文本|可视/流程/请求）+ 保存。始终完整显示，不随 Tab 滚动
  const headControls =
    !currentCasePath || binaryFile ? null : isConfig ? (
      <div className="tab-controls">
        <div className="view-switch">
          <button className={`vs-btn ${!configVisual ? "active" : ""}`} onClick={exitConfigVisual} title="原始 YAML">
            文本
          </button>
          <button className={`vs-btn ${configVisual ? "active" : ""}`} onClick={enterConfigVisual} title="可视化设置">
            可视
          </button>
        </div>
        <button className="save-btn ghost" onClick={saveCase} disabled={!dirty}>
          保存
        </button>
      </div>
    ) : (
      <div className="tab-controls">
        {caseEligible && (
          <div className="view-switch">
            <button className={`vs-btn ${effectiveText ? "active" : ""}`} onClick={onClickText} title="原始 YAML（互斥）">
              文本
            </button>
            {effectiveText ? (
              <button className="vs-btn" onClick={onClickVisual} title="可视化编辑">
                可视
              </button>
            ) : (
              <>
                <button className={`vs-btn ${showFlow ? "active" : ""}`} onClick={onClickFlow} title="DAG 流程画布">
                  流程
                </button>
                <button className={`vs-btn ${showRequest ? "active" : ""}`} onClick={onClickRequest} title="请求编辑器">
                  请求
                </button>
              </>
            )}
          </div>
        )}
        <button className="save-btn ghost" onClick={saveCase} disabled={!dirty}>
          保存
        </button>
      </div>
    );

  return (
    <div className="app">
      <header className={`topbar ${isFullscreen ? "is-fullscreen" : ""}`} data-tauri-drag-region>
        <div className="workspace-menu" ref={wsMenuRef}>
          <button
            className={`workspace-trigger ${workspace ? "" : "is-placeholder"} ${wsMenuOpen ? "is-open" : ""}`}
            onClick={() => setWsMenuOpen((v) => !v)}
          >
            <span className="workspace-label" title={workspace || undefined}>
              {workspace ? baseName(workspace) : "选择工作空间"}
            </span>
            <CaretDown open={wsMenuOpen} />
          </button>
          {wsMenuOpen && (
            <div className="workspace-dropdown">
              <button className="ws-item" onClick={openOrCreateWorkspace}>
                打开工作空间
              </button>
              <div className="ws-divider" />
              <div className="ws-section-title">最近</div>
              <div className="ws-recent-list">
                {recentWorkspaces.length === 0 ? (
                  <div className="ws-empty">暂无最近工作空间</div>
                ) : (
                  recentWorkspaces.map((ws) => (
                    <button key={ws} className="ws-item ws-recent" title={ws} onClick={() => selectWorkspace(ws)}>
                      <span className="ws-recent-name">{baseName(ws)}</span>
                      <span className="ws-recent-path">{ws}</span>
                    </button>
                  ))
                )}
              </div>
            </div>
          )}
        </div>

        {/* 右侧集群：环境 + 配置 + 三面板切换，整体靠最右 */}
        <div className="topbar-right">
        {workspace && (
          <div className="environment-menu" ref={envMenuRef}>
            <button className={`env-trigger ${envMenuOpen ? "is-open" : ""}`} onClick={() => setEnvMenuOpen((v) => !v)} title="切换环境">
              <span className="env-glyph">◇</span>
              <span className="env-label">{activeEnv || "无环境"}</span>
              <CaretDown open={envMenuOpen} />
            </button>
            {envMenuOpen && (
              <div className="workspace-dropdown env-dropdown">
                <div className="ws-section-title">环境</div>
                {Object.keys(environments).length === 0 ? (
                  <div className="ws-empty">application.yml 未配置环境</div>
                ) : (
                  Object.keys(environments).map((name) => (
                    <button
                      key={name}
                      className={`ws-item env-item ${name === activeEnv ? "active" : ""}`}
                      onClick={() => {
                        setActiveEnv(name);
                        setEnvMenuOpen(false);
                      }}
                    >
                      <span className="env-check">{name === activeEnv ? "✓" : ""}</span>
                      <span className="env-name">{name}</span>
                      <span className="env-count">{Object.keys(environments[name]).length} 变量</span>
                    </button>
                  ))
                )}
                <div className="ws-divider" />
                <button
                  className="ws-item"
                  onClick={() => {
                    setEnvMenuOpen(false);
                    const p = joinPath(workspace, "application.yml");
                    openTab(p);
                  }}
                >
                  编辑环境（application.yml）
                </button>
              </div>
            )}
          </div>
        )}

        {workspace && (
          <button
            className="topbar-config"
            title="工作空间配置（application.yml）"
            onClick={() => openTab(joinPath(workspace, "application.yml"))}
          >
            <ConfigIcon className="topbar-config-ico" size={18} />
          </button>
        )}

        {/* 右上角：三面板显隐切换（仿 VSCode） */}
        <div className="layout-toggles">
          <button
            className={`layout-toggle ${effectiveShowLeft ? "is-on" : ""}`}
            title="切换左侧边栏（文件树）"
            aria-pressed={effectiveShowLeft}
            onClick={toggleLeft}
          >
            <PanelIcon side="left" />
          </button>
          <button
            className={`layout-toggle ${showBottom ? "is-on" : ""}`}
            title="切换底部栏（终端）"
            aria-pressed={showBottom}
            onClick={toggleBottom}
          >
            <PanelIcon side="bottom" />
          </button>
          <button
            className={`layout-toggle ${showRight ? "is-on" : ""}`}
            title="切换右侧边栏（AI 对话）"
            aria-pressed={showRight}
            onClick={toggleRight}
          >
            <PanelIcon side="right" />
          </button>
        </div>
        </div>
      </header>

      <div className="body-layout">
        {/* 左侧栏：开启左栏即显示；有工作空间显示文件树，否则显示空态引导 */}
        {effectiveShowLeft && (
        <aside
          className="sidebar"
          style={{ width: sidebarWidth }}
          onContextMenu={(e) => {
            if (workspace) openContext(e, null);
          }}
        >
          {workspace ? (
            <>
              <div className="tree-toolbar">
                <div className="tree-search-wrap">
                  <span className="tree-search-icon">⌕</span>
                  <input
                    ref={searchInputRef}
                    className="tree-search"
                    placeholder="搜索用例…"
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                  />
                  {searchQuery && (
                    <button className="tree-search-clear" title="清空" onClick={() => setSearchQuery("")}>
                      ×
                    </button>
                  )}
                </div>
                <button
                  className="tree-add"
                  title="新建"
                  onClick={(e) => {
                    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                    setCtxMenu({ x: r.left, y: r.bottom + 4, entry: null });
                  }}
                >
                  +
                </button>
              </div>

              {searchQuery.trim() ? (
                <div className="search-results">
                  {searchResults.length === 0 ? (
                    <div className="search-empty">无匹配</div>
                  ) : (
                    searchResults.map((r) => {
                      const rel = relPath(workspace, dirName(r.path));
                      return (
                        <div
                          key={r.path}
                          className={`search-row ${currentCasePath === r.path ? "selected" : ""} ${r.isDir ? "is-dir" : ""}`}
                          title={r.path}
                          onClick={() => {
                            if (!r.isDir) onSelectFile(r.path);
                          }}
                          onContextMenu={(e) => openContext(e, r)}
                        >
                          {r.isDir ? <FolderIcon /> : isAppConfig(r.path) ? <ConfigIcon /> : <FileIcon active={isYamlFile(r.path)} />}
                          <span className="search-name">{r.name}</span>
                          {rel && rel !== r.name && <span className="search-path">{rel}</span>}
                        </div>
                      );
                    })
                  )}
                </div>
              ) : (
                <div className="tree">
                  <div className="tree-root-name" title={workspace}>
                    <FolderIcon />
                    <span className="tree-name">{baseName(workspace)}</span>
                  </div>
                  {(childrenMap[workspace] || []).map((c) => (
                    <TreeNode
                      key={c.path}
                      entry={c}
                      depth={0}
                      expanded={expanded}
                      childrenMap={childrenMap}
                      selectedPath={currentCasePath}
                      onToggle={toggleDir}
                      onSelect={onSelectFile}
                      onContext={openContext}
                    />
                  ))}
                </div>
              )}
            </>
          ) : (
            <div className="sidebar-empty">
              <div>未打开工作空间</div>
            </div>
          )}
        </aside>
        )}

        {effectiveShowLeft && (
          <div
            className="sidebar-resizer"
            onMouseDown={(e) => {
              e.preventDefault();
              resizingRef.current = true;
              document.body.classList.add("resizing-col", "resizing-sidebar");
            }}
          />
        )}

        {/* 中间列：主工作区 + 底部终端栏 */}
        <div className="center-col" ref={centerColRef}>
        {/* 主工作区 */}
        <main className="workspace">
          {tabOrder.length > 0 && (
            <div className="tab-row">
              <TabBar
                tabs={tabOrder}
                active={currentCasePath}
                isDirty={isDirtyPath}
                onSelect={openTab}
                onClose={closeTab}
                onContext={(e, p) => {
                  e.preventDefault();
                  e.stopPropagation();
                  setTabMenu({ x: e.clientX, y: e.clientY, path: p });
                }}
              />
              {headControls}
            </div>
          )}
          {!currentCasePath ? (
            <div className="workspace-empty">
              <img className="empty-logo" src="/nautilus.svg" alt="" draggable={false} />
              <div className="empty-shortcuts">
                {(workspace ? ACTIONS : ACTIONS.filter((a) => a.id === "open-workspace")).map((a) => {
                  const accel = scBindings[a.id];
                  if (!accel) return null;
                  return (
                    <div
                      key={a.id}
                      className="empty-sc-row"
                      role="button"
                      onClick={() => scActionsRef.current[a.id]?.()}
                    >
                      <span className="empty-sc-label">{a.label}</span>
                      <span className="empty-sc-keys">
                        {accelTokens(accel).map((t, i) => (
                          <kbd key={i} className="empty-sc-key">
                            {t}
                          </kbd>
                        ))}
                      </span>
                    </div>
                  );
                })}
              </div>
            </div>
          ) : binaryFile ? (
            <div className="binary-view">
              <svg className="binary-ico" viewBox="0 0 24 24" width="44" height="44" aria-hidden="true">
                <path fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" d="M6 3h8l4 4v14H6z" />
                <path fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" d="M14 3v4h4" />
                <path fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" d="M9.5 12.5 7.5 15l2 2.5M14.5 12.5l2 2.5-2 2.5" />
              </svg>
              <div className="binary-msg">此文件是二进制文件或使用了不受支持的文本编码，所以无法在文本编辑器中显示。</div>
            </div>
          ) : isConfig ? (
            <>
              {error && <div className="error-box">⚠ {error}</div>}
              {externalStale && (
                <div className="stale-box">
                  <span>⚠ 此文件已在外部被修改，而你有未保存的改动。</span>
                  <span className="stale-actions">
                    <button className="stale-btn reload" onClick={() => { setExternalStale(false); openCase(currentCasePath); }}>
                      重新加载
                    </button>
                    <button className="stale-btn" onClick={() => setExternalStale(false)}>
                      忽略
                    </button>
                  </span>
                </div>
              )}

              {configVisual ? (
                <SettingsPage
                  environments={environments}
                  onChange={onEnvChange}
                  workspacePath={workspace}
                  configPath={currentCasePath}
                  shortcutOverrides={scOverrides}
                  onShortcutChange={onShortcutChange}
                />
              ) : (
                <div className="text-view">
                  <div className="text-warn is-config">⚙ 工作空间配置文件（application.yml）——编辑环境后保存即生效。</div>
                  <textarea
                    className="raw-editor"
                    value={rawText}
                    spellCheck={false}
                    onChange={(e) => {
                      setRawText(e.target.value);
                      mark();
                    }}
                  />
                </div>
              )}
            </>
          ) : (
            <>
              {error && <div className="error-box">⚠ {error}</div>}
              {externalStale && (
                <div className="stale-box">
                  <span>⚠ 此文件已在外部被修改，而你有未保存的改动。</span>
                  <span className="stale-actions">
                    <button className="stale-btn reload" onClick={() => { setExternalStale(false); openCase(currentCasePath); }}>
                      重新加载
                    </button>
                    <button className="stale-btn" onClick={() => setExternalStale(false)}>
                      忽略
                    </button>
                  </span>
                </div>
              )}

              {effectiveText ? (
                <div className="text-view">
                  {!caseValid && caseEligible && (
                    // 仅 .yml/.yaml 才提示"不是有效用例"；普通 .txt/.json 干净地当文本
                    <div className="text-warn">⚠ 该文件不是有效用例（{textError || "缺少 requests"}）；以纯文本显示。</div>
                  )}
                  <textarea
                    className="raw-editor"
                    value={rawText}
                    spellCheck={false}
                    onChange={(e) => {
                      setRawText(e.target.value);
                      mark();
                    }}
                  />
                </div>
              ) : (
                <div ref={structuredRef} className={`structured ${showFlow && showRequest ? "split" : showFlow ? "only-flow" : "only-request"}`}>
                  {showFlow && (
                    <div
                      className="flow-pane"
                      style={
                        showFlow && showRequest && flowPaneWidth != null
                          ? { flex: `0 0 ${flowPaneWidth}px`, maxWidth: "none", minWidth: 0 }
                          : undefined
                      }
                    >
                      <FlowCanvas
                        nodes={flowNodes}
                        selectedId={selectedRequestId}
                        ui={uiNodes}
                        onSelect={setSelectedRequestId}
                        onAddStep={addRequest}
                        onDeleteStep={deleteRequest}
                        onRunAll={onRunAll}
                        running={runningAll}
                        onMoveNode={moveNode}
                        onConnect={addDependency}
                        onDisconnect={removeDependency}
                        onResetLayout={resetLayout}
                      />
                    </div>
                  )}
                  {showFlow && showRequest && selected && (
                    <div
                      className="pane-resizer"
                      onMouseDown={(e) => {
                        e.preventDefault();
                        flowResizingRef.current = true;
                        document.body.classList.add("resizing-col", "resizing-pane");
                      }}
                      onDoubleClick={() => setFlowPaneWidth(null)}
                      title="拖动调整宽度（双击恢复默认）"
                    />
                  )}
                  {showRequest && selected && (
                    <div className="request-pane">
                      <RequestEditor
                        key={currentCasePath + "/" + selectedRequestId}
                        value={selected.req}
                        onChange={updateReq}
                        onSend={() => onSendRequest(selected.id)}
                        sending={sending}
                        sendLabel="发送"
                        assertions={selected.assertions}
                        onAssertions={setAssertions}
                        assertResults={run?.asserts}
                        outputs={isFlow ? selected.outputs : undefined}
                        onOutputs={isFlow ? setOutputs : undefined}
                        stepId={selected.id}
                        onRenameId={isFlow ? (v) => renameRequest(selected.id, v) : undefined}
                      />

                      {/* 响应区 */}
                      <div className="response">
                        <div className="response-head">
                          <span className="response-title">响应</span>
                          {resp && (
                            <span className="response-meta">
                              <span className={`status-badge ${statusClass(resp.status)}`}>
                                {resp.status} {resp.statusText}
                              </span>
                              <span className="meta-item">{resp.elapsedMs} ms</span>
                              <span className="meta-item">{byteSize(resp.body)}</span>
                            </span>
                          )}
                        </div>

                        {runErr && <div className="error-box">⚠ {runErr}</div>}

                        {resp && (
                          <>
                            <div className="tabs sub">
                              <button className={`tab ${respTab === "body" ? "active" : ""}`} onClick={() => setRespTab("body")}>
                                响应体
                              </button>
                              <button className={`tab ${respTab === "headers" ? "active" : ""}`} onClick={() => setRespTab("headers")}>
                                响应头 ({resp.headers.length})
                              </button>
                              {respTab === "body" && (
                                <label className="pretty-toggle">
                                  <input type="checkbox" checked={pretty} onChange={(e) => setPretty(e.target.checked)} />
                                  美化
                                </label>
                              )}
                            </div>
                            <div className="tab-panel">
                              {respTab === "body" ? (
                                <pre className="response-body">{pretty ? prettyBody(resp.body) : resp.body}</pre>
                              ) : (
                                <table className="kv-table readonly">
                                  <tbody>
                                    {resp.headers.map((h, i) => (
                                      <tr key={i}>
                                        <td className="hk">{h.key}</td>
                                        <td className="hv">{h.value}</td>
                                      </tr>
                                    ))}
                                  </tbody>
                                </table>
                              )}
                            </div>
                          </>
                        )}

                        {!resp && !runErr && !sending && (
                          <div className="response-empty">填写请求并点击 发送 查看响应</div>
                        )}
                        {sending && <div className="response-empty">请求发送中…</div>}
                      </div>
                    </div>
                  )}
                </div>
              )}
            </>
          )}
        </main>

          {/* 底部终端栏：首次打开后常驻，隐藏用 display:none 保留 shell 会话与滚动 */}
          {termEverOpened.current && (
            <>
              <div
                className="panel-resizer-h"
                style={{ display: showBottom ? "block" : "none" }}
                onMouseDown={(e) => {
                  e.preventDefault();
                  bottomResizingRef.current = true;
                  document.body.classList.add("resizing-row");
                }}
              />
              <div className="bottom-panel" style={{ height: bottomHeight, display: showBottom ? "flex" : "none" }}>
                <div className="panel-head">
                  <span className="panel-head-title">
                    <span className="panel-head-glyph">›_</span> 终端
                  </span>
                  <span className="panel-head-actions">
                    <button className="panel-add" title="新建终端" onClick={addTerminal}>
                      +
                    </button>
                    <button className="panel-close" title="隐藏终端栏" onClick={() => setLayout((l) => ({ ...l, bottom: false }))}>
                      ×
                    </button>
                  </span>
                </div>
                <div className="bottom-panel-body">
                  <div className="term-stack">
                    {terminals.map((t) => {
                      const on = showBottom && activeTermId === t.id;
                      return (
                        <div key={t.id} className="term-pane-wrap" style={{ display: on ? "flex" : "none" }}>
                          <TerminalPane cwd={t.cwd} active={on} />
                        </div>
                      );
                    })}
                  </div>
                  {terminals.length > 0 && (
                    <div className="term-tabs">
                      {terminals.map((t) => (
                        <div
                          key={t.id}
                          className={`term-tab ${activeTermId === t.id ? "active" : ""}`}
                          title={`终端 ${t.n}`}
                          onClick={() => setActiveTermId(t.id)}
                        >
                          <span className="term-tab-glyph">›_</span>
                          <span className="term-tab-label">终端 {t.n}</span>
                          <button
                            className="term-tab-close"
                            title="关闭此终端"
                            onClick={(e) => {
                              e.stopPropagation();
                              closeTerminal(t.id);
                            }}
                          >
                            ×
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            </>
          )}
        </div>

        {/* 右侧 AI 对话栏：可拖宽；首次打开后常驻，隐藏保留对话历史 */}
        {showRight && (
          <div
            className="ai-resizer"
            onMouseDown={(e) => {
              e.preventDefault();
              aiResizingRef.current = true;
              document.body.classList.add("resizing-col", "resizing-ai");
            }}
          />
        )}
        {aiEverOpened.current && (
          <aside className="ai-panel" style={{ width: aiWidth, display: showRight ? "flex" : "none" }}>
            <AiChat />
          </aside>
        )}
      </div>

      {ctxMenu && <ContextMenu x={ctxMenu.x} y={ctxMenu.y} items={ctxItems(ctxMenu.entry)} onClose={() => setCtxMenu(null)} />}
      {tabMenu && (
        <ContextMenu
          x={tabMenu.x}
          y={tabMenu.y}
          items={[
            { label: "关闭当前标签页", onClick: () => closeTab(tabMenu.path) },
            { label: "关闭其他标签页", onClick: () => closeOtherTabs(tabMenu.path) },
            { label: "关闭全部标签页", onClick: () => closeAllTabs() },
          ]}
          onClose={() => setTabMenu(null)}
        />
      )}
      {promptState && (
        <PromptDialog
          title={promptState.title}
          initial={promptState.initial}
          onCancel={() => setPromptState(null)}
          onOk={(v) => {
            const fn = promptState.onOk;
            setPromptState(null);
            fn(v);
          }}
        />
      )}
      {newCaseDir !== null && (
        <NewCaseDialog
          onCancel={() => setNewCaseDir(null)}
          onOk={(v) => {
            const dir = newCaseDir;
            setNewCaseDir(null);
            createCaseFile(dir, v.name, v.method, v.url);
          }}
        />
      )}
    </div>
  );
}

export default App;
