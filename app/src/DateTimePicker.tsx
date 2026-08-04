// 日期时间选择器：只读输入框 + 弹出的月历面板。
//
// 不用原生 `<input type="datetime-local">`：它的外观各平台各不相同（WebKit 下是一串分段数字，
// 与全站其余控件完全不搭），也没法在控件内放「清除 / 今天」这类动作——而这恰恰是这里最常用的两下：
// 「这条 cookie 不要过期时间了」和「给我一个从今天算起的时间」。
//
// 值统一用 **Unix 毫秒**（与 `CookieItem.expiresMs` 对齐），`undefined` = 未设置。
import { useEffect, useRef, useState } from "react";
import { formatDateTime, monthGrid, sameDay } from "./datetime";

const WEEK = ["一", "二", "三", "四", "五", "六", "日"] as const;
const HOURS = Array.from({ length: 24 }, (_, i) => i);
const MINUTES = Array.from({ length: 60 }, (_, i) => i);

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** 把列内的选中项滚到可见处。用 scrollTop 而不是 scrollIntoView——后者会连带滚动祖先容器 */
function scrollToSelected(list: HTMLDivElement | null) {
  const el = list?.querySelector<HTMLElement>(".is-sel");
  if (!list || !el) return;
  list.scrollTop = el.offsetTop - list.clientHeight / 2 + el.offsetHeight / 2;
}

export function DateTimePicker({
  value,
  onChange,
  placeholder = "未设置",
  ariaLabel,
}: {
  value?: number;
  onChange: (ms?: number) => void;
  placeholder?: string;
  ariaLabel?: string;
}) {
  const [open, setOpen] = useState(false);
  const [view, setView] = useState(() => new Date(value ?? Date.now()));
  const wrapRef = useRef<HTMLDivElement>(null);
  const hourRef = useRef<HTMLDivElement>(null);
  const minRef = useRef<HTMLDivElement>(null);

  // 打开时把当前时分滚到列中间：60 个分钟项，不定位的话每次都得从 00 找起
  useEffect(() => {
    if (!open) return;
    scrollToSelected(hourRef.current);
    scrollToSelected(minRef.current);
  }, [open, value]);

  // 选中值变了（含外部清空）时把视图挪过去，免得下次打开还停在别的月份
  useEffect(() => {
    if (value) setView(new Date(value));
  }, [value]);

  // 点面板外 / 按 Esc 关闭（同 Select 的行为）
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation(); // 只关面板，不连带把整个对话框关掉
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  const sel = value ? new Date(value) : null;
  const today = new Date();

  /** 选日期时保留已选的时分；没选过就用当前时刻（取整到分） */
  function pickDay(d: Date) {
    const base = sel ?? new Date();
    onChange(new Date(d.getFullYear(), d.getMonth(), d.getDate(), base.getHours(), base.getMinutes()).getTime());
  }

  /** 选时分时保留已选的日期；没选过就落在今天 */
  function pickTime(h: number, m: number) {
    const base = sel ?? new Date();
    onChange(new Date(base.getFullYear(), base.getMonth(), base.getDate(), h, m).getTime());
  }

  return (
    <div className={`dtp ${open ? "is-open" : ""}`} ref={wrapRef}>
      <button
        type="button"
        className="dtp-trigger"
        aria-label={ariaLabel}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <svg className="dtp-ico" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
          <rect x="2.2" y="3.4" width="11.6" height="10.4" rx="1.6" fill="none" stroke="currentColor" strokeWidth="1.3" />
          <path fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" d="M2.2 6.6h11.6M5.4 2.2v2.2M10.6 2.2v2.2" />
        </svg>
        <span className={value ? "dtp-value" : "dtp-value is-empty"}>{value ? formatDateTime(value) : placeholder}</span>
      </button>

      {open && (
        <div className="dtp-panel" role="dialog">
          <div className="dtp-head">
            <button
              type="button"
              className="icon-btn"
              title="上一月"
              onClick={() => setView(new Date(view.getFullYear(), view.getMonth() - 1, 1))}
            >
              <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
                <path fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" d="M10 3.5 5.5 8 10 12.5" />
              </svg>
            </button>
            <span className="dtp-month">
              {view.getFullYear()} 年 {view.getMonth() + 1} 月
            </span>
            <button
              type="button"
              className="icon-btn"
              title="下一月"
              onClick={() => setView(new Date(view.getFullYear(), view.getMonth() + 1, 1))}
            >
              <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
                <path fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" d="M6 3.5 10.5 8 6 12.5" />
              </svg>
            </button>
          </div>

          <div className="dtp-body">
            <div className="dtp-cal">
              <div className="dtp-week">
                {WEEK.map((w) => (
                  <span key={w}>{w}</span>
                ))}
              </div>
              <div className="dtp-grid">
                {monthGrid(view).map((d) => {
                  const cls = [
                    d.getMonth() === view.getMonth() ? "" : "is-out", // 补位的上下月，淡一档但仍可点
                    sel && sameDay(d, sel) ? "is-sel" : "",
                    sameDay(d, today) ? "is-today" : "",
                  ]
                    .filter(Boolean)
                    .join(" ");
                  return (
                    <button type="button" key={d.getTime()} className={`dtp-day ${cls}`} onClick={() => pickDay(d)}>
                      {d.getDate()}
                    </button>
                  );
                })}
              </div>
            </div>

            {/* 时 / 分两列滚动选：比 `<input type="time">` 少一次「点进去再敲数字」，
                也免了各平台对 time 控件各画各的 */}
            <div className="dtp-clock">
              <div className="dtp-col">
                <div className="dtp-col-head">时</div>
                <div className="dtp-col-list" ref={hourRef}>
                  {HOURS.map((h) => (
                    <button
                      type="button"
                      key={h}
                      className={`dtp-tick ${sel && sel.getHours() === h ? "is-sel" : ""}`}
                      onClick={() => pickTime(h, sel?.getMinutes() ?? 0)}
                    >
                      {pad2(h)}
                    </button>
                  ))}
                </div>
              </div>
              <div className="dtp-col">
                <div className="dtp-col-head">分</div>
                <div className="dtp-col-list" ref={minRef}>
                  {MINUTES.map((m) => (
                    <button
                      type="button"
                      key={m}
                      className={`dtp-tick ${sel && sel.getMinutes() === m ? "is-sel" : ""}`}
                      onClick={() => pickTime(sel?.getHours() ?? 0, m)}
                    >
                      {pad2(m)}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          </div>

          <div className="dtp-actions">
            <button type="button" className="dtp-link" onClick={() => onChange(undefined)}>
              清除
            </button>
            {/* 「今天」给的是此刻：真实用途是「从现在起再撑一会儿」，给 00:00 反而是个已过去的时间 */}
            <button type="button" className="dtp-link" onClick={() => onChange(Date.now())}>
              今天
            </button>
            <button type="button" className="dtp-link is-primary" onClick={() => setOpen(false)}>
              完成
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
