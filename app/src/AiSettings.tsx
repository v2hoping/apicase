// 设置页「AI」分区：让 AI Agent 能直接操作这个工作空间。
//
// 三件事凑成一个闭环，缺一件 AI 就用不起来：
//   ① 命令行工具进 PATH —— AI 读了 AGENTS.md 会直接敲 `apicase`，敲不到就是硬失败
//   ② AGENTS.md         —— 告诉 AI 这是什么、有哪些命令、结果怎么读
//   ③ MCP（可选）        —— 另一条路，给只放行工具、不给 shell 的受管控环境
//
// 判断逻辑全在 Rust（`core::cli_link` / `core::agents`），这里只显示状态、转达点击。

import { useCallback, useEffect, useState } from "react";
import {
  type AiStatus,
  EMPTY_AI_STATUS,
  aiStatus,
  installCli,
  mcpConfigJson,
  uninstallCli,
  writeAgents,
} from "./ai";

/** 一行操作项：左边说明、右边按钮，状态用圆点表示。 */
function Row({
  ok,
  title,
  hint,
  action,
  disabled,
  onAction,
  danger,
}: {
  ok: boolean;
  title: string;
  hint: string;
  action: string;
  disabled?: boolean;
  onAction: () => void;
  danger?: boolean;
}) {
  return (
    <div className="ai-row">
      <span className={`ai-dot ${ok ? "is-on" : ""}`} aria-hidden="true" />
      <div className="ai-row-text">
        <div className="ai-row-title">{title}</div>
        <div className="ai-row-hint">{hint}</div>
      </div>
      <button className={`btn ${danger ? "is-danger" : ""}`} disabled={disabled} onClick={onAction}>
        {action}
      </button>
    </div>
  );
}

export default function AiSettings({
  workspace,
  autoSetup,
  onAutoSetupChange,
}: {
  workspace: string;
  autoSetup: boolean;
  onAutoSetupChange: (next: boolean) => void;
}) {
  const [st, setSt] = useState<AiStatus>(EMPTY_AI_STATUS);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  const [err, setErr] = useState("");
  const [copied, setCopied] = useState(false);

  // 进分区即重读一次：软链可能被外部删掉、AGENTS.md 可能被 git 操作改掉，
  // 缓存住只会显示过期状态（同 Cookies 分区的既有决策）
  const refresh = useCallback(() => {
    aiStatus(workspace).then(setSt).catch(() => {});
  }, [workspace]);
  useEffect(refresh, [refresh]);

  async function act(fn: () => Promise<unknown>, done: (r: unknown) => string) {
    setBusy(true);
    setErr("");
    setMsg("");
    try {
      setMsg(done(await fn()));
    } catch (e) {
      // 错误原样显示：「被别的 apicase 占用」「目录不可写」都是用户能自己处理的，
      // 吞成一句「失败」等于让他去猜
      setErr(String(e));
    } finally {
      setBusy(false);
      refresh();
    }
  }

  const installed = st.linkState === "installed";
  const unavailable = st.linkState === "unavailable";

  const linkHint = unavailable
    ? "这个构建里没有自带命令行工具"
    : st.linkState === "foreign"
      ? `${st.link} 是另一个 apicase，不是本应用自带的那份`
      : installed
        ? (st.link ?? "")
        : st.link
          ? `将装到 ${st.link}`
          : "找不到可用的安装位置";

  const config = mcpConfigJson(workspace);

  return (
    <div className="settings-section ai-settings">
      <p className="settings-lead">
        让 Claude Code、Cursor、Copilot 等 AI Agent 直接读写并运行这个工作空间的用例。
      </p>

      <Row
        ok={installed}
        title="命令行工具"
        hint={linkHint}
        action={installed ? "移除" : "安装"}
        danger={installed}
        disabled={busy || unavailable || st.linkState === "foreign"}
        onAction={() =>
          installed
            ? act(uninstallCli, () => "已从 PATH 移除")
            : act(installCli, () => "已安装，新开的终端里即可使用 apicase")
        }
      />
      {st.needsPathSetup && !installed && st.link && (
        <div className="ai-note">
          注意：{st.link.replace(/\/[^/]+$/, "")} 还不在 PATH 里，装完需要自行加入 shell 配置。
        </div>
      )}

      <Row
        ok={st.agents}
        title="AGENTS.md"
        hint={
          workspace
            ? st.agents
              ? "已就绪，随 git 走到每台机器"
              : "告诉 AI 这是什么、有哪些命令、结果怎么读"
            : "先打开一个工作空间"
        }
        action={st.agents ? "更新" : "生成"}
        disabled={busy || !workspace}
        onAction={() => act(() => writeAgents(workspace), (r) => String(r))}
      />

      <label className="ai-auto">
        <input type="checkbox" checked={autoSetup} onChange={(e) => onAutoSetupChange(e.target.checked)} />
        <span>
          打开工作空间时自动补齐
          <em>会在缺失时静默安装命令行工具并生成 AGENTS.md（后者随 git 走，请确认团队认可）</em>
        </span>
      </label>

      {msg && <div className="ai-msg is-ok">{msg}</div>}
      {err && <div className="ai-msg is-err">{err}</div>}

      <div className="ai-mcp">
        <div className="ai-row-title">MCP · 另一条路</div>
        <div className="ai-row-hint">
          AGENTS.md 零配置、覆盖三十多个 Agent；MCP 则要在每个客户端里配一次，
          适合只放行工具、不给 shell 的环境。把这段加进客户端配置：
        </div>
        <pre className="ai-code">{config}</pre>
        <button
          className="btn"
          onClick={() => {
            navigator.clipboard
              ?.writeText(config)
              .then(() => {
                setCopied(true);
                setTimeout(() => setCopied(false), 1600);
              })
              .catch(() => {});
          }}
        >
          {copied ? "已复制" : "复制"}
        </button>
      </div>
    </div>
  );
}
