// 批量运行参数的组装 —— 重点是**并行度**这一项。
//
// 并行度从工作空间设置（application.yml 的 settings.concurrency）一路递到执行内核。
// 这里钉的是前端这一段的归一化：0 会让内核的信号量容量为 0，表现是整轮运行永远拿不到
// 令牌而挂起——一个清空的输入框不该换来一个卡死的运行。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { batchRunOpts, debugRunOpts, clampConcurrency } = await loadModule("src/run.ts");
const { MAX_CONCURRENCY } = await loadModule("src/case.ts");

const env = { name: "dev", vars: {} };
const client = {};

// ── clampConcurrency ───────────────────────────────

eq(clampConcurrency(1), 1, "串行");
eq(clampConcurrency(4), 4, "常规值原样");
eq(clampConcurrency(0), 1, "0 会让信号量容量为 0 而挂起，必须兜成串行");
eq(clampConcurrency(-3), 1, "负数同理");
eq(clampConcurrency(2.9), 2, "小数向下取整");
eq(clampConcurrency(MAX_CONCURRENCY + 1), MAX_CONCURRENCY, "越界截到上限，而不是变成一次压测");
eq(clampConcurrency(NaN), 1, "空输入框 → Number('') 是 NaN，回落串行");
// 非有限值回落串行而非截到上限，与 Rust 侧 parse_settings 的 `filter(is_finite)` 一致：
// 两端对同一个坏值给出不同结果，就等于并行度有了两套规则
eq(clampConcurrency(Infinity), 1, "非有限值回落串行（同 Rust 侧）");

// ── batchRunOpts ───────────────────────────────────

eq(batchRunOpts(env, client).concurrency, 1, "不给就是串行——不改变既有行为");
eq(batchRunOpts(env, client, false, 6).concurrency, 6, "工作空间设置的值要真的递下去");
eq(batchRunOpts(env, client, false, 0).concurrency, 1, "组装时同样兜住 0");
eq(batchRunOpts(env, client, false, 999).concurrency, MAX_CONCURRENCY, "组装时同样截上限");

// 失败传播与并行度正交：一个管 case 内部谁该跑，一个管 case 之间跑几条
ok(batchRunOpts(env, client, true, 4).continueOnAssertionFailure, "两项互不干扰");

// ── 调试运行恒串行 ─────────────────────────────────
//
// 界面上的「发送 / ▶ 运行全部」跑的是一个 case 内部的 step，它们之间有 dependsOn 与
// outputs 传递，并发跑没有意义。并行度只作用于 case 之间。
eq(debugRunOpts(env, client).concurrency, 1, "调试运行不受并行度影响");

report();
