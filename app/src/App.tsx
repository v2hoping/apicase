import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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

function byteSize(s: string): string {
  const bytes = new Blob([s]).size;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

function App() {
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
      {/* 顶部品牌栏 */}
      <header className="topbar">
        <span className="brand">apicase</span>
        <span className="brand-sub">API 调试</span>
      </header>

      <div className="body-layout">
        {/* 左侧文件树占位（folder = Collection，后续需求实现） */}
        <aside className="sidebar">
          <div className="sidebar-title">Collections</div>
          <div className="sidebar-placeholder">
            打开一个目录即项目
            <br />
            （文件树后续实现）
          </div>
        </aside>

        {/* 主工作区 */}
        <main className="workspace">
          {/* 请求行 */}
          <div className="request-bar">
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
