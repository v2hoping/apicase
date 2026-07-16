// 轻量测试运行器：用 esbuild 把 TS/TSX 源打包成 ESM 后动态导入，直接测真实实现（零新增依赖）。
import * as esbuild from "esbuild";

export async function loadModule(entry) {
  const result = await esbuild.build({
    entryPoints: [entry],
    bundle: true,
    format: "esm",
    platform: "node",
    write: false,
    logLevel: "silent",
    jsx: "automatic",
  });
  const code = result.outputFiles[0].text;
  return import("data:text/javascript," + encodeURIComponent(code));
}

let pass = 0;
let fail = 0;
const failures = [];

export function eq(actual, expected, msg) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a === e) {
    pass++;
  } else {
    fail++;
    failures.push(`✗ ${msg}\n    期望: ${e}\n    实际: ${a}`);
  }
}

export function ok(cond, msg) {
  if (cond) pass++;
  else {
    fail++;
    failures.push(`✗ ${msg}`);
  }
}

// 断言子串包含 / 不包含
export function has(hay, needle, msg) {
  ok(typeof hay === "string" && hay.includes(needle), `${msg}（应包含 ${JSON.stringify(needle)}，实际 ${JSON.stringify(hay)}）`);
}
export function hasnt(hay, needle, msg) {
  ok(typeof hay === "string" && !hay.includes(needle), `${msg}（不应包含 ${JSON.stringify(needle)}，实际 ${JSON.stringify(hay)}）`);
}

export function report() {
  console.log(`\n通过 ${pass} · 失败 ${fail}`);
  if (fail) {
    console.log("\n失败详情：");
    for (const f of failures) console.log(f);
    process.exit(1);
  } else {
    console.log("全部通过 ✓");
  }
}
