import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
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

function byteSize(s: string): string {
  const bytes = new Blob([s]).size;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

// 文件树节点（递归渲染，支持展开/折叠）
function TreeNode({
  entry,
  depth,
  expanded,
  childrenMap,
  selectedPath,
  onToggle,
  onSelect,
}: {
  entry: DirEntry;
  depth: number;
  expanded: Set<string>;
  childrenMap: Record<string, DirEntry[]>;
  selectedPath: string;
  onToggle: (entry: DirEntry) => void;
  onSelect: (path: string) => void;
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
            />
          ))}
        </div>
      )}
    </div>
  );
}

function App() {
  // 工作空间：打开的目录即一个工作空间（workspace）。
  // workspace 为当前选中的工作空间；recentWorkspaces 为最近打开的列表（后续接入打开目录后填充）。
  const [workspace, setWorkspace] = useState("");
  const [recentWorkspaces, setRecentWorkspaces] = useState<string[]>([]);
  const [wsMenuOpen, setWsMenuOpen] = useState(false);
  const wsMenuRef = useRef<HTMLDivElement>(null);
  // 全屏时红绿灯消失，下拉应贴左（padding 收窄）；退出全屏恢复留白
  const [isFullscreen, setIsFullscreen] = useState(false);
  // 左侧栏宽度（可拖动调节）
  const [sidebarWidth, setSidebarWidth] = useState(200);
  const resizingRef = useRef(false);
  // 文件树：childrenMap 缓存各目录子项（懒加载），expanded 记录已展开目录
  const [childrenMap, setChildrenMap] = useState<Record<string, DirEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selectedPath, setSelectedPath] = useState("");

  const [method, setMethod] = useState("GET");
  const [url, setUrl] = useState("https://example.com");
  const [headers, setHeaders] = useState<HeaderEntry[]>([{ key: "", value: "" }]);
  const [body, setBody] = useState("");
  const [reqTab, setReqTab] = useState<"headers" | "body">("headers");

  const [resp, setResp] = useState<ApiResponse | null>(null);
  const [respTab, setRespTab] = useState<"body" | "headers">("body");
  const [pretty, setPretty] = useState(true);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  // 左侧栏拖动调宽：拖动时监听全局 mousemove/mouseup
  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!resizingRef.current) return;
      // sidebar 紧贴窗口左缘，clientX 即为目标宽度，限制在 [160, 480]
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

  // 全屏切换：用窗口尺寸的「变化方向」判断，比 isFullscreen() 更及时——
  // 退出全屏时窗口从动画一开始就在缩小，可立即恢复留白（早于红绿灯出现，避免覆盖）；
  // 放大/不变时再查 isFullscreen() 确认是否进入全屏（进入保持瞬时贴左）。
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
            // 窗口在缩小 → 正在退出全屏 → 立即恢复留白
            setIsFullscreen(false);
            return;
          }
          // 放大/不变 → 查询确认是否进入全屏
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

  // 读取某目录的直接子项并缓存
  async function loadDir(path: string) {
    try {
      const entries = await invoke<DirEntry[]>("list_dir", { path });
      setChildrenMap((prev) => ({ ...prev, [path]: entries }));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  // 展开/折叠目录；首次展开时懒加载其子项
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

  // 选中某个工作空间：设为当前、置顶到最近列表（去重，最多 10 条），并加载其文件树
  function applyWorkspace(path: string) {
    setWorkspace(path);
    setRecentWorkspaces((prev) => [path, ...prev.filter((p) => p !== path)].slice(0, 10));
    setChildrenMap({});
    setExpanded(new Set());
    setSelectedPath("");
    loadDir(path);
  }

  // 打开或创建工作空间：选择一个目录；init_workspace 幂等——
  // 目录已有 application.yml 即视为打开，否则写入并创建。
  async function openOrCreateWorkspace() {
    setWsMenuOpen(false);
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "打开或创建工作空间",
      });
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

  function updateHeader(i: number, field: "key" | "value", val: string) {
    setHeaders((prev) => {
      const next = prev.map((h, idx) => (idx === i ? { ...h, [field]: val } : h));
      // 末行被填写时自动追加一空行
      if (i === prev.length - 1 && (next[i].key || next[i].value)) {
        next.push({ key: "", value: "" });
      }
      return next;
    });
  }

  function removeHeader(i: number) {
    setHeaders((prev) => prev.filter((_, idx) => idx !== i));
  }

  async function send() {
    if (!url.trim()) {
      setError("请先填写 URL");
      return;
    }
    setLoading(true);
    setError(null);
    setResp(null);
    const request: ApiRequest = {
      method,
      url: url.trim(),
      headers: headers.filter((h) => h.key.trim() !== ""),
      body: body.trim() === "" ? null : body,
    };
    try {
      const result = await invoke<ApiResponse>("send_request", { request });
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
      {/* 顶部栏：作为窗口可拖动区域；macOS Overlay 模式下左侧留出红绿灯位置 */}
      <header
        className={`topbar ${isFullscreen ? "is-fullscreen" : ""}`}
        data-tauri-drag-region
      >
        <div className="workspace-menu" ref={wsMenuRef}>
          <button
            className={`workspace-trigger ${workspace ? "" : "is-placeholder"} ${
              wsMenuOpen ? "is-open" : ""
            }`}
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
                    <button
                      key={ws}
                      className="ws-item ws-recent"
                      title={ws}
                      onClick={() => selectWorkspace(ws)}
                    >
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
        {/* 左侧栏：文件树占位（folder = Collection，后续需求实现） */}
        <aside className="sidebar" style={{ width: sidebarWidth }}>
          {workspace ? (
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
                  onSelect={setSelectedPath}
                />
              ))}
            </div>
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

        {/* 拖动手柄：调节左侧栏宽度 */}
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
          {/* 请求行 */}
          <div className="request-bar">
            {/* method + url 连体（Postman 风格），共用一个边框 */}
            <div className="url-group">
              <select
                className={`method-select ${methodClass(method)}`}
                value={method}
                onChange={(e) => setMethod(e.target.value)}
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
                onChange={(e) => setUrl(e.target.value)}
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
            <button
              className={`tab ${reqTab === "headers" ? "active" : ""}`}
              onClick={() => setReqTab("headers")}
            >
              Headers
            </button>
            <button
              className={`tab ${reqTab === "body" ? "active" : ""}`}
              onClick={() => setReqTab("body")}
            >
              Body
            </button>
          </div>

          <div className="tab-panel">
            {reqTab === "headers" ? (
              <table className="kv-table">
                <thead>
                  <tr>
                    <th>Key</th>
                    <th>Value</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {headers.map((h, i) => (
                    <tr key={i}>
                      <td>
                        <input
                          value={h.key}
                          placeholder="Header"
                          onChange={(e) => updateHeader(i, "key", e.target.value)}
                        />
                      </td>
                      <td>
                        <input
                          value={h.value}
                          placeholder="Value"
                          onChange={(e) => updateHeader(i, "value", e.target.value)}
                        />
                      </td>
                      <td className="op-cell">
                        {headers.length > 1 && (
                          <button className="row-del" onClick={() => removeHeader(i)}>
                            ×
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ) : (
              <textarea
                className="body-input"
                value={body}
                placeholder={'请求体，例如 {"name":"apicase"}'}
                onChange={(e) => setBody(e.target.value)}
              />
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
                  <button
                    className={`tab ${respTab === "body" ? "active" : ""}`}
                    onClick={() => setRespTab("body")}
                  >
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
                      <input
                        type="checkbox"
                        checked={pretty}
                        onChange={(e) => setPretty(e.target.checked)}
                      />
                      Pretty
                    </label>
                  )}
                </div>
                <div className="tab-panel">
                  {respTab === "body" ? (
                    <pre className="response-body">
                      {pretty ? prettyBody(resp.body) : resp.body}
                    </pre>
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

            {!resp && !error && !loading && (
              <div className="response-empty">填写请求并点击 Send 查看响应</div>
            )}
          </div>
        </main>
      </div>
    </div>
  );
}

export default App;
