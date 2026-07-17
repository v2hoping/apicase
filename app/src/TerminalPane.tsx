// 底部终端面板：xterm.js 前端 + Rust portable-pty 后端，体验对标 VSCode 内置终端。
// 首次挂载即在工作空间目录起一个交互 shell；输出经 `terminal://data/{id}` 事件流入、
// 输入经 terminal_write 写回、面板尺寸变化经 terminal_resize 同步行列。
import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

// 浅色终端主题：白底 + 深字 + 黑光标，ANSI 色为浅底优化
const LIGHT_THEME = {
  background: "#ffffff",
  foreground: "#1c1c1e",
  cursor: "#1c1c1e",
  cursorAccent: "#ffffff",
  selectionBackground: "rgba(45,127,249,0.18)",
  black: "#1c1c1e",
  red: "#d1383d",
  green: "#2e9e6b",
  yellow: "#b8860b",
  blue: "#2f6fdb",
  magenta: "#b3309b",
  cyan: "#1f8ea3",
  white: "#8a8a8e",
  brightBlack: "#8a8a8e",
  brightRed: "#e5484d",
  brightGreen: "#3bb078",
  brightYellow: "#c99a2e",
  brightBlue: "#4a86e8",
  brightMagenta: "#c94fb5",
  brightCyan: "#2aa7bd",
  brightWhite: "#1c1c1e",
};

// 深色终端主题：深底 + 浅字 + 亮光标，ANSI 色为深底提亮
const DARK_THEME = {
  background: "#17171a",
  foreground: "#e6e6ea",
  cursor: "#e6e6ea",
  cursorAccent: "#17171a",
  selectionBackground: "rgba(77,148,255,0.3)",
  black: "#3b3b42",
  red: "#f0554f",
  green: "#40bd6a",
  yellow: "#d9a83c",
  blue: "#4d94ff",
  magenta: "#c96fd0",
  cyan: "#3fb6cc",
  white: "#c8c8ce",
  brightBlack: "#6a6a72",
  brightRed: "#ff6b66",
  brightGreen: "#5fd486",
  brightYellow: "#e8bf5a",
  brightBlue: "#6ba6ff",
  brightMagenta: "#dd8fe0",
  brightCyan: "#5fcfe0",
  brightWhite: "#f4f4f8",
};

const themeObj = (t: "light" | "dark") => (t === "dark" ? DARK_THEME : LIGHT_THEME);

let SEQ = 0;

export function TerminalPane({ cwd, active, theme }: { cwd: string; active: boolean; theme: "light" | "dark" }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const idRef = useRef<string>("");
  const themeRef = useRef(theme);

  // 挂载即创建终端与后端会话；卸载即关闭。cwd 变化（切工作空间）也重建。
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const id = `term-${++SEQ}-${Math.floor(performance.now())}`;
    idRef.current = id;

    const term = new Terminal({
      fontSize: 12.5,
      fontFamily:
        'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
      lineHeight: 1.15,
      cursorBlink: true,
      theme: themeObj(themeRef.current),
      allowProposedApi: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    termRef.current = term;
    fitRef.current = fit;

    let disposed = false;
    let unlistenData: UnlistenFn | null = null;
    let unlistenExit: UnlistenFn | null = null;

    // 先订阅输出事件，再 open 会话，避免早期输出丢失
    (async () => {
      unlistenData = await listen<number[]>(`terminal://data/${id}`, (e) => {
        term.write(new Uint8Array(e.payload));
      });
      unlistenExit = await listen(`terminal://exit/${id}`, () => {
        term.write("\r\n\x1b[38;5;244m[进程已结束]\x1b[0m\r\n");
      });
      if (disposed) return;

      try {
        fit.fit();
      } catch {
        /* 面板此刻可能不可见，稍后 active 时再 fit */
      }
      const cols = term.cols || 80;
      const rows = term.rows || 24;
      await invoke("terminal_open", { id, cwd, cols, rows }).catch((err) => {
        term.write(`\r\n\x1b[31m无法启动终端: ${err}\x1b[0m\r\n`);
      });
    })();

    // 键盘输入写回后端
    const onData = term.onData((data) => {
      invoke("terminal_write", { id: idRef.current, data }).catch(() => {});
    });

    // 面板尺寸变化：refit 并把新行列同步给 PTY
    const ro = new ResizeObserver(() => {
      const t = termRef.current;
      const f = fitRef.current;
      if (!t || !f) return;
      if (host.clientWidth === 0 || host.clientHeight === 0) return;
      try {
        f.fit();
        invoke("terminal_resize", { id: idRef.current, cols: t.cols, rows: t.rows }).catch(() => {});
      } catch {
        /* ignore */
      }
    });
    ro.observe(host);

    return () => {
      disposed = true;
      ro.disconnect();
      onData.dispose();
      unlistenData?.();
      unlistenExit?.();
      invoke("terminal_close", { id }).catch(() => {});
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [cwd]);

  // 主题切换：热更新 xterm 配色（不重建会话，保留终端内容）
  useEffect(() => {
    themeRef.current = theme;
    if (termRef.current) termRef.current.options.theme = themeObj(theme);
  }, [theme]);

  // 从隐藏切回可见：此前 display:none 尺寸为 0，需重新 fit + focus
  useEffect(() => {
    if (!active) return;
    const t = termRef.current;
    const f = fitRef.current;
    if (!t || !f) return;
    const raf = requestAnimationFrame(() => {
      try {
        f.fit();
        invoke("terminal_resize", { id: idRef.current, cols: t.cols, rows: t.rows }).catch(() => {});
      } catch {
        /* ignore */
      }
      t.focus();
    });
    return () => cancelAnimationFrame(raf);
  }, [active]);

  return <div className="terminal-host" ref={hostRef} />;
}
