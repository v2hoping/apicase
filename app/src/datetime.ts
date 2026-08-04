// 日期时间的纯函数：显示格式与月历排布。
//
// 与 `DateTimePicker.tsx` 分开，一是让日历的排布逻辑（跨月补位、周一为首）能被单测钉住，
// 二是让 `cookies.ts` 这类非 UI 模块能复用同一份显示格式——列表与编辑框的时间对不上号，
// 最容易让人以为自己改错了。

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** Unix 毫秒 → `2026-09-01 10:30`（本地时区）。 */
export function formatDateTime(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** `HH:MM`（本地时区）——`<input type="time">` 的值。 */
export function formatTime(ms: number): string {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/**
 * 月视图的 42 个格子：从当月 1 号**所在那一周的周一**起排六周。
 *
 * 固定 42 格而不是按月份长度伸缩——否则切月时面板高度会跳。
 * 首列是周一（`getDay()` 里 0 是周日，故把周日折到第 7 位）。
 */
export function monthGrid(view: Date): Date[] {
  const first = new Date(view.getFullYear(), view.getMonth(), 1);
  const lead = (first.getDay() + 6) % 7;
  const start = new Date(first.getFullYear(), first.getMonth(), 1 - lead);
  return Array.from(
    { length: 42 },
    (_, i) => new Date(start.getFullYear(), start.getMonth(), start.getDate() + i),
  );
}
