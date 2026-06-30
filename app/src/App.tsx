import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Case,
  RequestSpec,
  BodySpec,
  AuthSpec,
  AuthType,
  BodyType,
  KV,
  parseCase,
  dumpCase,
  splitQueryFromUrl,
  mergeQueryIntoUrl,
} from "./case";
import "./App.css";

interface HeaderEntry {
  key: string;
  value: string;
}

interface ApiRequest {
  method: string;
  url: string;
  headers: HeaderEntry[];
  body: string | null;
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

const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const BODY_TYPES: BodyType[] = ["none", "json", "text", "form-urlencoded", "form-data"];
const AUTH_TYPES: AuthType[] = ["none", "bearer", "basic", "apikey"];

function methodClass(m: string): string {
  return `method-${m.toLowerCase()}`;
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

// case 文件：.yml/.yaml 且非工作空间根配置 application.yml
function isCaseFile(path: string): boolean {
  const n = baseName(path).toLowerCase();
  if (!n.endsWith(".yml") && !n.endsWith(".yaml")) return false;
  return n !== "application.yml" && n !== "application.yaml";
}

function byteSize(s: string): string {
  const bytes = new Blob([s]).size;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

// 通用键值表格（query / headers / 表单项复用）：末行填写自动追加空行，每行可勾选启用
function KVTable({
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
          <th>Key</th>
          <th>Value</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {display.map((r, i) => {
          const filled = !!(r.name || r.value);
          return (
            <tr key={i}>
              <td className="ck-col">
                <input
                  type="checkbox"
                  checked={r.enabled !== false}
                  onChange={(e) => update(i, { enabled: e.target.checked })}
                />
              </td>
              <td>
                <input
                  value={r.name}
                  placeholder={namePlaceholder}
                  onChange={(e) => update(i, { name: e.target.value })}
                />
              </td>
              <td>
                <input
                  value={r.value}
                  placeholder={valuePlaceholder}
                  onChange={(e) => update(i, { value: e.target.value })}
                />
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
  return (
    <div className="tree-node">
      <div
        className={`tree-row ${selectedPath === entry.path ? "selected" : ""}`}
        style={{ paddingLeft: 6 + depth * 14 }}
        title={entry.name}
        onClick={() => (entry.isDir ? onToggle(entry) : onSelect(entry.path))}
        onContextMenu={(e) => onContext(e, entry)}
      >
        <span className={`tree-caret ${entry.isDir ? "" : "tree-caret-empty"}`}>
          {entry.isDir ? (isOpen ? "▾" : "▸") : "▸"}
        </span>
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

// 文本输入对话框（新建 / 重命名用）
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
  const [name, setName] = useState("new-case");
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
        <div className="modal-title">新建 case</div>
        <div className="field-row">
          <label>名称</label>
          <input
            ref={ref}
            value={name}
            placeholder="case 名（.yml 可省略）"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submit();
              else if (e.key === "Escape") onCancel();
            }}
          />
        </div>
        <div className="field-row">
          <label>请求</label>
          <select className={`nc-method ${methodClass(method)}`} value={method} onChange={(e) => setMethod(e.target.value)}>
            {METHODS.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
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

function App() {
  // 工作空间
  const [workspace, setWorkspace] = useState("");
  const [recentWorkspaces, setRecentWorkspaces] = useState<string[]>([]);
  const [wsMenuOpen, setWsMenuOpen] = useState(false);
  const wsMenuRef = useRef<HTMLDivElement>(null);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(200);
  const resizingRef = useRef(false);
  // 文件树
  const [childrenMap, setChildrenMap] = useState<Record<string, DirEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selectedPath, setSelectedPath] = useState("");
  // 搜索栏 / 可视化新建
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<DirEntry[]>([]);
  const [newCaseDir, setNewCaseDir] = useState<string | null>(null);
  // 右键菜单 / 输入对话框
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; entry: DirEntry | null } | null>(null);
  const [promptState, setPromptState] = useState<{ title: string; initial: string; onOk: (v: string) => void } | null>(
    null,
  );

  // 当前打开的 case
  const [currentCasePath, setCurrentCasePath] = useState("");
  const [caseKind, setCaseKind] = useState<"none" | "single" | "flow">("none");
  const [caseName, setCaseName] = useState("");
  const [caseVars, setCaseVars] = useState<Record<string, unknown> | undefined>(undefined);
  const [caseVersion, setCaseVersion] = useState("0.1");
  const [dirty, setDirty] = useState(false);

  // 请求编辑态
  const [method, setMethod] = useState("GET");
  const [url, setUrl] = useState("https://example.com");
  const [query, setQuery] = useState<KV[]>([]);
  const [headers, setHeaders] = useState<KV[]>([]);
  const [reqTab, setReqTab] = useState<"params" | "headers" | "auth" | "body">("params");
  // auth
  const [authType, setAuthType] = useState<AuthType>("none");
  const [authBearerToken, setAuthBearerToken] = useState("");
  const [authBasicUser, setAuthBasicUser] = useState("");
  const [authBasicPass, setAuthBasicPass] = useState("");
  const [authApikeyKey, setAuthApikeyKey] = useState("");
  const [authApikeyValue, setAuthApikeyValue] = useState("");
  const [authApikeyIn, setAuthApikeyIn] = useState<"header" | "query">("header");
  // body
  const [bodyType, setBodyType] = useState<BodyType>("none");
  const [bodyText, setBodyText] = useState("");
  const [bodyContentType, setBodyContentType] = useState("");
  const [bodyForm, setBodyForm] = useState<KV[]>([]);

  const [resp, setResp] = useState<ApiResponse | null>(null);
  const [respTab, setRespTab] = useState<"body" | "headers">("body");
  const [pretty, setPretty] = useState(true);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const mark = () => setDirty(true);

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

  // 左侧栏拖动调宽
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!resizingRef.current) return;
      setSidebarWidth(Math.min(480, Math.max(160, e.clientX)));
    }
    function onUp() {
      if (!resizingRef.current) return;
      resizingRef.current = false;
      document.body.classList.remove("resizing-col");
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

  // Cmd/Ctrl+S 保存（用 ref 取最新 saveCase 闭包）
  const saveRef = useRef<() => void>(() => {});
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        saveRef.current();
      }
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

  function applyWorkspace(path: string) {
    setWorkspace(path);
    setRecentWorkspaces((prev) => [path, ...prev.filter((p) => p !== path)].slice(0, 10));
    setChildrenMap({});
    setExpanded(new Set());
    setSelectedPath("");
    closeCase();
    loadDir(path);
  }

  async function openOrCreateWorkspace() {
    setWsMenuOpen(false);
    try {
      const selected = await open({ directory: true, multiple: false, title: "打开或创建工作空间" });
      if (typeof selected === "string") {
        await invoke("init_workspace", { path: selected });
        applyWorkspace(selected);
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  function selectWorkspace(ws: string) {
    applyWorkspace(ws);
    setWsMenuOpen(false);
  }

  // ── case 打开 / 关闭 ────────────────────────────
  function closeCase() {
    setCurrentCasePath("");
    setCaseKind("none");
    setCaseName("");
    setCaseVars(undefined);
    setDirty(false);
  }

  function onSelectFile(path: string) {
    setSelectedPath(path);
    if (isCaseFile(path)) openCase(path);
  }

  async function openCase(path: string) {
    try {
      const text = await invoke<string>("read_text_file", { path });
      const c = parseCase(text);
      setCurrentCasePath(path);
      setCaseName(c.name || "");
      setCaseVars(c.vars);
      setCaseVersion(c.version);
      setCaseKind(c.kind);
      setDirty(false);
      setError(null);
      setResp(null);
      if (c.kind === "single" && c.request) {
        const r = c.request;
        setMethod(r.method);
        const split = splitQueryFromUrl(r.url);
        const allQuery = [...split.query, ...r.query];
        setUrl(mergeQueryIntoUrl(split.base, allQuery));
        setQuery(allQuery);
        setHeaders(r.headers);
        // auth
        setAuthType(r.auth.type);
        setAuthBearerToken(r.auth.bearer?.token || "");
        setAuthBasicUser(r.auth.basic?.username || "");
        setAuthBasicPass(r.auth.basic?.password || "");
        setAuthApikeyKey(r.auth.apikey?.key || "");
        setAuthApikeyValue(r.auth.apikey?.value || "");
        setAuthApikeyIn(r.auth.apikey?.in || "header");
        // body
        setBodyType(r.body.type);
        setBodyContentType(r.body.contentType || "");
        if (r.body.type === "json") {
          setBodyText(r.body.json === undefined ? "" : JSON.stringify(r.body.json, null, 2));
        } else if (r.body.type === "text") {
          setBodyText(r.body.text || "");
        } else {
          setBodyText("");
        }
        setBodyForm(r.body.type === "form-urlencoded" ? r.body.urlencoded || [] : r.body.type === "form-data" ? r.body.formData || [] : []);
        setReqTab("params");
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  function buildAuthSpec(): AuthSpec {
    if (authType === "bearer") return { type: "bearer", bearer: { token: authBearerToken } };
    if (authType === "basic") return { type: "basic", basic: { username: authBasicUser, password: authBasicPass } };
    if (authType === "apikey")
      return { type: "apikey", apikey: { key: authApikeyKey, value: authApikeyValue, in: authApikeyIn } };
    return { type: "none" };
  }

  function buildBodySpec(): BodySpec | null {
    if (bodyType === "json") {
      if (bodyText.trim() === "") return { type: "none" };
      try {
        return { type: "json", json: JSON.parse(bodyText) };
      } catch {
        setError("Body JSON 格式非法，无法保存");
        return null;
      }
    }
    if (bodyType === "text") return { type: "text", text: bodyText, contentType: bodyContentType || undefined };
    if (bodyType === "form-urlencoded") return { type: "form-urlencoded", urlencoded: bodyForm };
    if (bodyType === "form-data") return { type: "form-data", formData: bodyForm };
    return { type: "none" };
  }

  async function saveCase() {
    if (!currentCasePath || caseKind !== "single") return;
    const body = buildBodySpec();
    if (!body) return; // JSON 非法已提示
    const req: RequestSpec = {
      method,
      url: splitQueryFromUrl(url.trim()).base,
      query,
      headers,
      auth: buildAuthSpec(),
      body,
    };
    const c: Case = {
      version: caseVersion || "0.1",
      name: caseName || undefined,
      vars: caseVars,
      kind: "single",
      request: req,
    };
    try {
      await invoke("write_text_file", { path: currentCasePath, content: dumpCase(c) });
      setDirty(false);
      setError(null);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }
  saveRef.current = () => {
    if (currentCasePath && caseKind === "single" && dirty) saveCase();
  };

  // ── 文件管理（右键菜单触发）─────────────────────
  // 打开可视化新建对话框（目标目录）
  function newCaseIn(dir: string) {
    setNewCaseDir(dir);
  }

  async function createCaseFile(dir: string, name: string, method: string, url: string) {
    let fname = name.trim() || "new-case";
    if (!/\.(yml|yaml)$/i.test(fname)) fname += ".yml";
    const path = joinPath(dir, fname);
    const split = splitQueryFromUrl(url.trim());
    const c: Case = {
      version: "0.1",
      kind: "single",
      request: { method, url: split.base, query: split.query, headers: [], auth: { type: "none" }, body: { type: "none" } },
    };
    try {
      await invoke("create_file", { path, content: dumpCase(c) });
      await loadDir(dir);
      setExpanded((prev) => new Set(prev).add(dir));
      openCase(path);
      setSelectedPath(path);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  function newFolderIn(dir: string) {
    setPromptState({
      title: "新建 folder 名称",
      initial: "new-folder",
      onOk: async (name) => {
        if (!name.trim()) return;
        const path = joinPath(dir, name.trim());
        try {
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
          await invoke("rename_path", { from: entry.path, to });
          await loadDir(dir);
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
      await invoke("delete_path", { path: entry.path });
      await loadDir(dir);
      if (currentCasePath === entry.path || currentCasePath.startsWith(entry.path + "/")) {
        closeCase();
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  function ctxItems(entry: DirEntry | null) {
    const dir = entry ? (entry.isDir ? entry.path : dirName(entry.path)) : workspace;
    const items: { label: string; onClick: () => void; danger?: boolean }[] = [
      { label: "新建 case", onClick: () => newCaseIn(dir) },
      { label: "新建 folder", onClick: () => newFolderIn(dir) },
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

  // ── 请求编辑态变更（带 dirty）──────────────────
  function onUrlChange(raw: string) {
    setUrl(raw);
    setQuery(splitQueryFromUrl(raw).query);
    mark();
  }
  function onQueryChange(next: KV[]) {
    setQuery(next);
    setUrl(mergeQueryIntoUrl(url, next));
    mark();
  }

  // 组装实际发送的请求（合并 auth、body）
  function buildApiRequest(): ApiRequest {
    let finalUrl = url.trim();
    const hdrs: HeaderEntry[] = headers
      .filter((h) => h.enabled !== false && h.name.trim() !== "")
      .map((h) => ({ key: h.name.trim(), value: h.value }));
    const hasContentType = () => hdrs.some((h) => h.key.toLowerCase() === "content-type");
    // auth
    if (authType === "bearer" && authBearerToken) {
      hdrs.push({ key: "Authorization", value: `Bearer ${authBearerToken}` });
    } else if (authType === "basic") {
      hdrs.push({ key: "Authorization", value: `Basic ${btoa(`${authBasicUser}:${authBasicPass}`)}` });
    } else if (authType === "apikey" && authApikeyKey) {
      if (authApikeyIn === "header") {
        hdrs.push({ key: authApikeyKey, value: authApikeyValue });
      } else {
        const cur = splitQueryFromUrl(finalUrl);
        finalUrl = mergeQueryIntoUrl(finalUrl, [...cur.query, { name: authApikeyKey, value: authApikeyValue, enabled: true }]);
      }
    }
    // body
    let bodyStr: string | null = null;
    if (bodyType === "json" && bodyText.trim() !== "") {
      bodyStr = bodyText;
      if (!hasContentType()) hdrs.push({ key: "Content-Type", value: "application/json" });
    } else if (bodyType === "text" && bodyText !== "") {
      bodyStr = bodyText;
      if (bodyContentType && !hasContentType()) hdrs.push({ key: "Content-Type", value: bodyContentType });
    } else if (bodyType === "form-urlencoded") {
      const pairs = bodyForm.filter((k) => k.enabled !== false && k.name.trim() !== "");
      if (pairs.length) {
        bodyStr = pairs.map((k) => `${encodeURIComponent(k.name)}=${encodeURIComponent(k.value)}`).join("&");
        if (!hasContentType()) hdrs.push({ key: "Content-Type", value: "application/x-www-form-urlencoded" });
      }
    }
    // form-data 的 multipart 发送暂未实现（保存可用），此处不组装 body
    return { method, url: finalUrl, headers: hdrs, body: bodyStr };
  }

  async function send() {
    if (!url.trim()) {
      setError("请先填写 URL");
      return;
    }
    setLoading(true);
    setError(null);
    setResp(null);
    try {
      const result = await invoke<ApiResponse>("send_request", { request: buildApiRequest() });
      setResp(result);
      setRespTab("body");
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setLoading(false);
    }
  }

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
            <span className="workspace-caret">▾</span>
          </button>
          {wsMenuOpen && (
            <div className="workspace-dropdown">
              <button className="ws-item" onClick={openOrCreateWorkspace}>
                打开或创建工作空间
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
      </header>

      <div className="body-layout">
        {/* 左侧栏：文件树 */}
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
                    className="tree-search"
                    placeholder="搜索 case…"
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
                          className={`search-row ${selectedPath === r.path ? "selected" : ""} ${r.isDir ? "is-dir" : ""}`}
                          title={r.path}
                          onClick={() => {
                            if (!r.isDir) onSelectFile(r.path);
                          }}
                          onContextMenu={(e) => openContext(e, r)}
                        >
                          <span className="tree-caret">{r.isDir ? "▸" : "·"}</span>
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
                    {baseName(workspace)}
                  </div>
                  {(childrenMap[workspace] || []).map((c) => (
                    <TreeNode
                      key={c.path}
                      entry={c}
                      depth={0}
                      expanded={expanded}
                      childrenMap={childrenMap}
                      selectedPath={selectedPath}
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
              未找到工作空间.
              <br />
              <button className="link-btn" onClick={openOrCreateWorkspace}>
                创建或打开
              </button>
              工作空间.
            </div>
          )}
        </aside>

        <div
          className="sidebar-resizer"
          onMouseDown={(e) => {
            e.preventDefault();
            resizingRef.current = true;
            document.body.classList.add("resizing-col");
          }}
        />

        {/* 主工作区 */}
        <main className="workspace">
          {currentCasePath && (
            <div className="case-head">
              <span className="case-name">{baseName(currentCasePath)}</span>
              {dirty && <span className="dirty-dot" title="未保存" />}
              {caseKind === "single" && (
                <button className="save-btn ghost" onClick={saveCase} disabled={!dirty}>
                  保存
                </button>
              )}
            </div>
          )}

          {caseKind === "flow" ? (
            <div className="flow-placeholder">
              <div className="flow-title">这是一个多节点 flow</div>
              <div className="flow-sub">flow 可视化编辑即将支持，当前版本暂不渲染。</div>
            </div>
          ) : (
            <>
              {/* 请求行 */}
              <div className="request-bar">
                <div className="url-group">
                  <select
                    className={`method-select ${methodClass(method)}`}
                    value={method}
                    onChange={(e) => {
                      setMethod(e.target.value);
                      mark();
                    }}
                  >
                    {METHODS.map((m) => (
                      <option key={m} value={m}>
                        {m}
                      </option>
                    ))}
                  </select>
                  <input
                    className="url-input"
                    value={url}
                    placeholder="https://api.example.com/path"
                    onChange={(e) => onUrlChange(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") send();
                    }}
                  />
                </div>
                <button className="send-btn" onClick={send} disabled={loading}>
                  {loading ? "发送中…" : "Send"}
                </button>
              </div>

              {/* 请求配置 Tabs */}
              <div className="tabs">
                {(["params", "headers", "auth", "body"] as const).map((t) => (
                  <button key={t} className={`tab ${reqTab === t ? "active" : ""}`} onClick={() => setReqTab(t)}>
                    {t === "params" ? "Params" : t === "headers" ? "Headers" : t === "auth" ? "Auth" : "Body"}
                  </button>
                ))}
              </div>

              <div className="tab-panel">
                {reqTab === "params" && (
                  <KVTable rows={query} onChange={onQueryChange} namePlaceholder="参数名" valuePlaceholder="参数值" />
                )}
                {reqTab === "headers" && (
                  <KVTable
                    rows={headers}
                    onChange={(rows) => {
                      setHeaders(rows);
                      mark();
                    }}
                    namePlaceholder="Header"
                    valuePlaceholder="Value"
                  />
                )}
                {reqTab === "auth" && (
                  <div className="auth-panel">
                    <div className="field-row">
                      <label>类型</label>
                      <select
                        value={authType}
                        onChange={(e) => {
                          setAuthType(e.target.value as AuthType);
                          mark();
                        }}
                      >
                        {AUTH_TYPES.map((t) => (
                          <option key={t} value={t}>
                            {t}
                          </option>
                        ))}
                      </select>
                    </div>
                    {authType === "bearer" && (
                      <div className="field-row">
                        <label>Token</label>
                        <input
                          value={authBearerToken}
                          placeholder="{{token}}"
                          onChange={(e) => {
                            setAuthBearerToken(e.target.value);
                            mark();
                          }}
                        />
                      </div>
                    )}
                    {authType === "basic" && (
                      <>
                        <div className="field-row">
                          <label>Username</label>
                          <input
                            value={authBasicUser}
                            onChange={(e) => {
                              setAuthBasicUser(e.target.value);
                              mark();
                            }}
                          />
                        </div>
                        <div className="field-row">
                          <label>Password</label>
                          <input
                            value={authBasicPass}
                            onChange={(e) => {
                              setAuthBasicPass(e.target.value);
                              mark();
                            }}
                          />
                        </div>
                      </>
                    )}
                    {authType === "apikey" && (
                      <>
                        <div className="field-row">
                          <label>Key</label>
                          <input
                            value={authApikeyKey}
                            onChange={(e) => {
                              setAuthApikeyKey(e.target.value);
                              mark();
                            }}
                          />
                        </div>
                        <div className="field-row">
                          <label>Value</label>
                          <input
                            value={authApikeyValue}
                            onChange={(e) => {
                              setAuthApikeyValue(e.target.value);
                              mark();
                            }}
                          />
                        </div>
                        <div className="field-row">
                          <label>位置</label>
                          <select
                            value={authApikeyIn}
                            onChange={(e) => {
                              setAuthApikeyIn(e.target.value as "header" | "query");
                              mark();
                            }}
                          >
                            <option value="header">header</option>
                            <option value="query">query</option>
                          </select>
                        </div>
                      </>
                    )}
                    {authType === "none" && <div className="panel-hint">无鉴权</div>}
                  </div>
                )}
                {reqTab === "body" && (
                  <div className="body-panel">
                    <div className="body-type-bar">
                      <select
                        value={bodyType}
                        onChange={(e) => {
                          setBodyType(e.target.value as BodyType);
                          mark();
                        }}
                      >
                        {BODY_TYPES.map((t) => (
                          <option key={t} value={t}>
                            {t}
                          </option>
                        ))}
                      </select>
                      {bodyType === "text" && (
                        <input
                          className="ct-input"
                          value={bodyContentType}
                          placeholder="Content-Type（可选）"
                          onChange={(e) => {
                            setBodyContentType(e.target.value);
                            mark();
                          }}
                        />
                      )}
                    </div>
                    {bodyType === "none" && <div className="panel-hint">无 Body</div>}
                    {(bodyType === "json" || bodyType === "text") && (
                      <textarea
                        className="body-input"
                        value={bodyText}
                        placeholder={bodyType === "json" ? '{"name":"apicase"}' : "请求体文本"}
                        onChange={(e) => {
                          setBodyText(e.target.value);
                          mark();
                        }}
                      />
                    )}
                    {(bodyType === "form-urlencoded" || bodyType === "form-data") && (
                      <KVTable
                        rows={bodyForm}
                        onChange={(rows) => {
                          setBodyForm(rows);
                          mark();
                        }}
                        namePlaceholder="字段名"
                        valuePlaceholder="字段值"
                      />
                    )}
                    {bodyType === "form-data" && <div className="panel-hint">form-data 发送暂仅支持文本字段</div>}
                  </div>
                )}
              </div>

              {/* 响应区 */}
              <div className="response">
                <div className="response-head">
                  <span className="response-title">Response</span>
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

                {error && <div className="error-box">⚠ {error}</div>}

                {resp && (
                  <>
                    <div className="tabs sub">
                      <button className={`tab ${respTab === "body" ? "active" : ""}`} onClick={() => setRespTab("body")}>
                        Body
                      </button>
                      <button
                        className={`tab ${respTab === "headers" ? "active" : ""}`}
                        onClick={() => setRespTab("headers")}
                      >
                        Headers ({resp.headers.length})
                      </button>
                      {respTab === "body" && (
                        <label className="pretty-toggle">
                          <input type="checkbox" checked={pretty} onChange={(e) => setPretty(e.target.checked)} />
                          Pretty
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

                {!resp && !error && !loading && <div className="response-empty">填写请求并点击 Send 查看响应</div>}
              </div>
            </>
          )}
        </main>
      </div>

      {ctxMenu && (
        <ContextMenu x={ctxMenu.x} y={ctxMenu.y} items={ctxItems(ctxMenu.entry)} onClose={() => setCtxMenu(null)} />
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
