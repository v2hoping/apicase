// 右侧 AI 对话面板（本期为 UI 外壳）：消息气泡列表 + 多行输入。
// 发送后追加用户消息与一条占位助手回复；接入真实模型时只需替换 reply 来源。
import { useEffect, useRef, useState } from "react";

interface Msg {
  role: "user" | "assistant";
  text: string;
}

const GREETING: Msg = {
  role: "assistant",
  text: "你好，我是 apicase 助手。当前为界面预览版——接入模型后我就能帮你分析接口、生成用例、解释响应了。",
};

export function AiChat() {
  const [msgs, setMsgs] = useState<Msg[]>([GREETING]);
  const [input, setInput] = useState("");
  const listRef = useRef<HTMLDivElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);

  // 新消息滚动到底
  useEffect(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [msgs]);

  function send() {
    const text = input.trim();
    if (!text) return;
    const reply: Msg = {
      role: "assistant",
      text: "（占位回复）AI 尚未接入，这里将来会返回真实回答。你刚才说：\n" + text,
    };
    setMsgs((m) => [...m, { role: "user", text }, reply]);
    setInput("");
    // 重置输入框高度
    if (taRef.current) taRef.current.style.height = "auto";
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    // Enter 发送，Shift+Enter 换行
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  return (
    <div className="ai-chat">
      <div className="ai-head">
        <span className="ai-title">
          <span className="ai-glyph">✦</span> AI 对话
        </span>
        <button className="ai-clear" title="清空对话" onClick={() => setMsgs([GREETING])}>
          清空
        </button>
      </div>

      <div className="ai-messages" ref={listRef}>
        {msgs.map((m, i) => (
          <div key={i} className={`ai-msg ${m.role}`}>
            <div className="ai-avatar">{m.role === "user" ? "你" : "✦"}</div>
            <div className="ai-bubble">{m.text}</div>
          </div>
        ))}
      </div>

      <div className="ai-input-row">
        <textarea
          ref={taRef}
          className="ai-input"
          placeholder="询问 AI…（Enter 发送，Shift+Enter 换行）"
          value={input}
          rows={1}
          spellCheck={false}
          onChange={(e) => {
            setInput(e.target.value);
            // 随内容自增高（上限 5 行）
            const ta = e.target;
            ta.style.height = "auto";
            ta.style.height = Math.min(ta.scrollHeight, 120) + "px";
          }}
          onKeyDown={onKeyDown}
        />
        <button className="ai-send" disabled={!input.trim()} onClick={send} title="发送">
          发送
        </button>
      </div>
    </div>
  );
}
