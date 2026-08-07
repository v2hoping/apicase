// 设置页「AI」分区：让 AI Agent 能直接操作这个工作空间。
//
// 两件事凑成一个闭环，缺一件 AI 就用不起来：
//   ① 命令行工具进 PATH —— AI 读了 AGENTS.md 会直接敲 `apicase`，敲不到就是硬失败
//   ② AGENTS.md         —— 告诉 AI 这是什么、有哪些命令、结果怎么读
// 界面上把它们合成**一个「初始化」动作**：用户要的是「能用」，
// 拆成两行两个按钮只是把内部实现摊给他看。下方两行状态才是拆开的——
// 出问题时要知道是哪一件没成。
//
// 打开工作空间时**恒自动初始化**（不再有自动 / 手动开关）：apicase 的用例本就是给 AI
// 接管着写的，一个「要不要让 AI 能用」的选项只会制造「AI 说找不到 apicase 命令」这类求助。
// 按钮仍留着——自动那次可能失败（目录只读），状态也可能被外部改坏。
//
// MCP 是另一条路（给只放行工具、不给 shell 的受管控环境），故单独一块，
// 且给的是**各家客户端各自的接入命令**——`apicase mcp -w …` 是给客户端 spawn 的，
// 用户自己敲它只会得到一个卡住的 stdio 进程。
//
// 判断逻辑全在 Rust（`core::cli_link` / `core::agents`），这里只显示状态、转达点击。

import { useCallback, useEffect, useState } from "react";
import { Select } from "./RequestEditor";
import {
  type AgentsState,
  type AiStatus,
  EMPTY_AI_STATUS,
  MCP_CLIENTS,
  aiStatus,
  installCli,
  writeAgents,
} from "./ai";

/**
 * 一行状态：名目 + 值（+ 可选的尾注）。
 *
 * 值本身就是状态（路径 / 已配置 / 未配置），既不加状态点也不着色——
 * 这两行是「现在是什么样」的事实陈述，不是需要抢注意力的告警。
 */
function StatusRow({
  label,
  value,
  mono,
  note,
}: {
  label: string;
  value: string;
  /** 值是路径：用等宽字体、单行截断（全文进 title） */
  mono?: boolean;
  note?: string;
}) {
  return (
    <div className="ai-state-row">
      <span className="ai-state-k">{label}</span>
      <span className={`ai-state-v ${mono ? "is-mono" : ""}`} title={mono ? value : undefined}>
        {value}
      </span>
      {note && <span className="ai-state-note">{note}</span>}
    </div>
  );
}

/**
 * 配置片段里把 `mark` 那一段标红（要删掉的部分），其余按正文色显示。
 *
 * 配置文件多半还有别的内容，整块标红会读成「这个文件要整份删掉」——
 * 标出 apicase 这一项才是实情。找不到 `mark` 就整段标红（那时它本来就只有这一项）。
 */
function Marked({ text, mark }: { text: string; mark?: string }) {
  const i = mark ? text.indexOf(mark) : -1;
  if (i < 0) {
    return <span className="ai-del">{text}</span>;
  }
  return (
    <>
      {text.slice(0, i)}
      <span className="ai-del">{mark}</span>
      {text.slice(i + (mark as string).length)}
    </>
  );
}

/** AGENTS.md 的三态 → 文案。`stale` 单独成一态：显示「已配置」会让人以为 AI 拿的是新说明。 */
const AGENTS_TEXT: Record<AgentsState, { text: string; note?: string }> = {
  ready: { text: "已配置" },
  stale: { text: "不一致", note: "点「初始化」更新" },
  absent: { text: "未配置" },
};

export default function AiSettings({ workspace }: { workspace: string }) {
  const [st, setSt] = useState<AiStatus>(EMPTY_AI_STATUS);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  const [err, setErr] = useState("");
  const [client, setClient] = useState(MCP_CLIENTS[0]);
  const [way, setWay] = useState<"cli" | "file">("cli");
  const [copied, setCopied] = useState("");

  // 进分区即重读一次：软链可能被外部删掉、AGENTS.md 可能被 git 操作改掉，
  // 缓存住只会显示过期状态（同 Cookies 分区的既有决策）
  const refresh = useCallback(() => {
    aiStatus(workspace).then(setSt).catch(() => {});
  }, [workspace]);
  useEffect(refresh, [refresh]);

  const installed = st.linkState === "installed";
  const foreign = st.linkState === "foreign";
  const unavailable = st.linkState === "unavailable";
  const agents = AGENTS_TEXT[st.agentsState];
  // 没有命令行入口的客户端一律落在「文件配置」上。派生而不是用 effect 同步——
  // 后者会让切换 Agent 的那一帧显示上一家的内容
  const activeWay = client.cli ? way : "file";

  /**
   * 初始化：缺什么补什么。
   *
   * 两件事分别记结果再一起报——「CLI 装好了但 AGENTS.md 写失败」是真实存在的一种结局
   * （工作空间只读），只报一句「失败」会让人以为两件都没成。
   */
  async function init() {
    setBusy(true);
    setErr("");
    setMsg("");
    const done: string[] = [];
    try {
      if (!installed && !unavailable && !foreign) {
        await installCli();
        done.push("命令行工具已装进 PATH");
      }
      if (workspace) {
        // 已是最新时这一步返回「已是最新」，照样显示——用户点了按钮就该看到发生了什么
        done.push(await writeAgents(workspace));
      }
      setMsg(done.length ? done.join("；") : "已就绪，无需初始化");
    } catch (e) {
      // 错误原样显示：「被别的 apicase 占用」「目录不可写」都是用户能自己处理的，
      // 吞成一句「失败」等于让他去猜
      setErr(String(e));
      if (done.length) setMsg(done.join("；"));
    } finally {
      setBusy(false);
      refresh();
    }
  }

  // 「装到哪儿」只在没装时才有意义（那时 link 是建议落点，不是既成事实）
  const cli: { value: string; mono?: boolean; note?: string } = unavailable
    ? { value: "这个构建未自带命令行工具" }
    : installed || foreign
      ? { value: st.link ?? "已配置", mono: true }
      : { value: "未配置", note: st.link ? `将装到 ${st.link}` : undefined };

  /** 两块代码各有一个复制按钮，故记的是「刚复制的是哪一块」而不是一个布尔 */
  function copy(key: string, text: string) {
    navigator.clipboard
      ?.writeText(text)
      .then(() => {
        setCopied(key);
        setTimeout(() => setCopied(""), 1600);
      })
      .catch(() => {});
  }

  /** 一段可复制的代码：单行命令或多行配置片段 */
  function Snippet({ id, text, block }: { id: string; text: string; block?: boolean }) {
    return (
      <div className={`ai-cmd ${block ? "is-block" : ""}`}>
        <pre>{text}</pre>
        <button className="btn-ghost sm" onClick={() => copy(id, text)}>
          {copied === id ? "已复制" : "复制"}
        </button>
      </div>
    );
  }

  return (
    <div className="settings-section ai-settings">
      <p className="settings-lead">
        支持 Claude Code、Codex、OpenCode、Trae、Qoder 等完全接管操作该工作空间用例，包括编写、调试、运行等
      </p>

      <div className="ai-card">
        <div className="ai-row">
          <div className="ai-row-text">
            <div className="ai-row-title">初始化环境变量和 AGENTS</div>
          </div>
          {/* 按钮留着：自动那次可能因为目录只读之类失败，也可能状态被外部改坏，得有个手动重来的入口 */}
          <button className="btn-primary sm" disabled={busy} onClick={init}>
            初始化
          </button>
        </div>

        <div className="ai-state">
          <StatusRow label="CLI 环境变量" {...cli} />
          <StatusRow
            label="AGENTS.md"
            value={workspace ? agents.text : "先打开一个工作空间"}
            note={workspace ? agents.note : undefined}
          />
          {st.needsPathSetup && !installed && st.link && (
            <div className="ai-note">
              {st.link.replace(/\/[^/]+$/, "")} 还不在 PATH 里，装完需要自行加进 shell 配置。
            </div>
          )}
          {msg && <div className="ai-msg is-ok">{msg}</div>}
          {err && <div className="ai-msg is-err">{err}</div>}
        </div>
      </div>

      <div className="ai-card">
        <div className="ai-row">
          <div className="ai-row-text">
            <div className="ai-row-title">MCP</div>
          </div>
          {/* 下拉而不是并排分段：Agent 会越加越多，一排按钮迟早把标题挤没 */}
          <Select
            className="ai-client-select"
            ariaLabel="选择 Agent"
            value={client.id}
            options={MCP_CLIENTS.map((c) => ({ value: c.id, label: c.label }))}
            onChange={(id) => setClient(MCP_CLIENTS.find((c) => c.id === id) ?? MCP_CLIENTS[0])}
          />
        </div>

        {/* 两种配置**二选一**，故做成标签页而不是上下摞两个代码块——后者读起来
            像是要依次都做一遍。IDE 型客户端（Trae / Qoder）没有命令行入口，
            那个标签就禁用并说明原因，不让人去找一条不存在的命令 */}
        <div className="ai-tabs" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={activeWay === "cli"}
            className={`ai-tab ${activeWay === "cli" ? "is-on" : ""}`}
            disabled={!client.cli}
            title={client.cli ? undefined : `${client.label} 没有命令行入口`}
            onClick={() => setWay("cli")}
          >
            命令配置
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={activeWay === "file"}
            className={`ai-tab ${activeWay === "file" ? "is-on" : ""}`}
            onClick={() => setWay("file")}
          >
            文件配置
          </button>
        </div>

        {/* 接入与删除对称地各占一步——装完发现不对却找不到怎么撤，比装不上更让人恼火。
            命令这边不另起说明行：落点与作用域写在命令上方的 # 注释里，复制时一并带走 */}
        {activeWay === "cli" && client.cli ? (
          <>
            <div className="ai-step">接入</div>
            <Snippet id="cli-add" text={client.cli.add} block />
            <div className="ai-step is-remove">删除</div>
            <Snippet id="cli-remove" text={client.cli.remove} block />
          </>
        ) : (
          <>
            {/* 有落点要交代才占这一行——JSON 片段里写不了注释，只能摆在它上方 */}
            {(client.file.where || !client.cli) && (
              <div className="ai-where">
                {client.file.where}
                {!client.cli && `${client.file.where ? "　" : ""}（${client.label} 没有命令行入口，只能这样配）`}
              </div>
            )}
            <div className="ai-step">接入</div>
            <Snippet id="file" text={client.file.snippet} block />

            {/* 删除区展示的就是**要拿掉的那一段**，比一句「把 xx 那项删掉」的说明直观得多。
                只把 apicase 那一项标红：整块标红会读成「这个文件要整份删掉」。
                面板型客户端文件里没有这段，那里给一句操作说明 */}
            <div className="ai-step is-remove">删除</div>
            <div className={`ai-cmd is-block ${client.file.removeText ? "is-text" : ""}`}>
              <pre>
                {client.file.removeText ? (
                  client.file.removeText
                ) : (
                  <Marked text={client.file.snippet} mark={client.file.removeMark} />
                )}
              </pre>
            </div>
            {client.file.removeNote && <div className="ai-cmd-note">{client.file.removeNote}</div>}
          </>
        )}
      </div>
    </div>
  );
}
