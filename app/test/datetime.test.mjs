// 日期时间纯函数：显示格式与月历排布。
// 排布错一天，用户点「1 号」拿到的就是上个月的最后一天——这类错屏幕上很难看出来，用例钉住。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { formatDateTime, formatTime, sameDay, monthGrid } = await loadModule("src/datetime.ts");

// ── 显示格式（本地时区）──
const t = new Date(2026, 8, 1, 10, 30).getTime(); // 2026-09-01 10:30
eq(formatDateTime(t), "2026-09-01 10:30", "年月日时分，两位补零");
eq(formatTime(t), "10:30", "时间控件的值");
eq(formatDateTime(new Date(2026, 0, 5, 9, 5).getTime()), "2026-01-05 09:05", "个位数月日时分都补零");
eq(formatDateTime(NaN), "—", "坏值不显示 Invalid Date");

// ── sameDay ──
ok(sameDay(new Date(2026, 8, 1, 0, 0), new Date(2026, 8, 1, 23, 59)), "同一天的不同时刻");
ok(!sameDay(new Date(2026, 8, 1), new Date(2026, 8, 2)), "相邻两天");
ok(!sameDay(new Date(2025, 8, 1), new Date(2026, 8, 1)), "跨年同月同日");

// ── monthGrid ──
// 2026-09-01 是周二 → 首格应是 8/31（周一）
const sep = monthGrid(new Date(2026, 8, 15));
eq(sep.length, 42, "恒 42 格（六周），切月时面板高度才不会跳");
eq(formatDateTime(sep[0].getTime()).slice(0, 10), "2026-08-31", "从当月 1 号所在那一周的周一起排");
eq(sep[0].getDay(), 1, "首列是周一");
eq(sep[6].getDay(), 0, "末列是周日");
eq(formatDateTime(sep[41].getTime()).slice(0, 10), "2026-10-11", "补到第六周结束");

// 1 号正好是周一时不补前导（2026-06-01 是周一）
const jun = monthGrid(new Date(2026, 5, 10));
eq(formatDateTime(jun[0].getTime()).slice(0, 10), "2026-06-01", "1 号是周一则首格就是它");

// 1 号是周日时要补满 6 格（2026-02-01 是周日）
const feb = monthGrid(new Date(2026, 1, 10));
eq(formatDateTime(feb[0].getTime()).slice(0, 10), "2026-01-26", "1 号是周日则补 6 天前导");
eq(feb[6].getDate(), 1, "第 7 格才是 1 号");

// 闰年二月：29 号要在格子里
const leap = monthGrid(new Date(2024, 1, 10));
ok(
  leap.some((d) => d.getMonth() === 1 && d.getDate() === 29),
  "2024 年 2 月有 29 号",
);

report();
