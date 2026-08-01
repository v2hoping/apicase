// 报告推送的增量决策。
//
// live 运行时每完成一个 case 都会往 iframe 里 postMessage 一次，而结构化克隆的开销按
// **整份报告**的大小走：每次都重发整份，跑 N 个用例就是 O(N²) 的复制量（一份 500 用例、
// 每步带 64KB 响应预览的报告可达几十 MB）。只推新增那一条则是 O(N)。
//
// 这里钉的是"什么时候不能走增量"——判断错了，报告页看到的会是一份缺东西的报告。
import { loadModule, eq, report } from "./harness.mjs";

const { reportPush } = await loadModule("src/run.ts");

const rep = (n, status = "running") => ({
  cases: Array.from({ length: n }, (_, i) => ({ file: `c${i}.yml` })),
  status,
});

// ── 正常增量 ───────────────────────────────────────

eq(reportPush({ runId: "r1", count: 3 }, "r1", rep(5)), { kind: "cases", from: 3 }, "只推第 3、4 条");
eq(reportPush({ runId: "r1", count: 4 }, "r1", rep(5)), { kind: "cases", from: 4 }, "一次新增一条是常态");
eq(reportPush({ runId: "r1", count: 5 }, "r1", rep(5)), { kind: "cases", from: 5 }, "无新增：from == 长度，调用方不发消息");

// ── 必须退回整份的四种情况 ──────────────────────────

eq(reportPush(null, "r1", rep(2)), { kind: "full" }, "首次推送");
eq(reportPush({ runId: "r0", count: 9 }, "r1", rep(2)), { kind: "full" }, "换了另一份报告——按 r0 的进度增量会丢掉 r1 的前 2 条");
eq(reportPush({ runId: "r1", count: 5 }, "r1", rep(2)), { kind: "full" }, "case 变少（重跑 / 读回历史）");
eq(reportPush({ runId: "r1", count: 3 }, "r1", rep(5, "done")), { kind: "full" }, "运行收尾要带上 status/finishedAt，且只发生一次");
eq(reportPush({ runId: "r1", count: 3 }, "r1", rep(5, "cancelled")), { kind: "full" }, "取消同样是终态");

// 历史报告（一开就是终态）恒走整份
eq(reportPush(null, "file:/a.html", rep(8, "done")), { kind: "full" }, "打开历史报告");

report();
