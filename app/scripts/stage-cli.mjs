// 把命令行二进制备到 `src-tauri/bin/`，供 Tauri 打包时作为 resource 收进安装包。
//
// # 为什么要有个中转目录
//
// `tauri.conf.json` 里的 resources 路径是写死的一条，而 dev 与打包用的是不同 profile
// （dev 用 debug 图快，打包用 release）。直接把配置指向 `../target/release/apicase`
// 的话，只 build 过 debug 的机器上 `npm run tauri dev` 会因为那个文件不存在而失败。
// 中转一层，配置就只认 `bin/apicase` 这一个位置，谁填进去由这里决定。
//
// # 为什么不用 externalBin
//
// Tauri 的 `externalBin` 会把文件放进 `Contents/MacOS/`，而桌面端自己在那儿叫
// `Apicase`（productName）——macOS 与 Windows 的文件系统**默认大小写不敏感**，
// `apicase` 与 `Apicase` 在同一个目录里是同一个文件，会静默互相覆盖。
// 放进 `Resources/bin/` 就是不同目录，撞不上了。
//
//   用法：node scripts/stage-cli.mjs [--debug]

import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const APP = dirname(dirname(fileURLToPath(import.meta.url)));
const debug = process.argv.includes("--debug");
const profile = debug ? "debug" : "release";
const exe = process.platform === "win32" ? "apicase.exe" : "apicase";

// ① 先编。放在这里而不是让调用方自己记着编——忘了编的后果是「打出来的包里
//    装着上一次的旧 CLI」，而且不会有任何报错。
execFileSync("cargo", ["build", "-p", "apicase-cli", ...(debug ? [] : ["--release"])], {
  cwd: APP,
  stdio: "inherit",
});

// ② 复制到中转目录
const src = join(APP, "target", profile, exe);
const dstDir = join(APP, "src-tauri", "bin");
const dst = join(dstDir, exe);
mkdirSync(dstDir, { recursive: true });
copyFileSync(src, dst);

// ③ 补可执行位。`copyFileSync` 在多数平台会带过来，但打包链路里少一个 +x
//    的表现是「装好之后 apicase 敲不动」，代价太高，显式设一次更稳。
if (process.platform !== "win32") chmodSync(dst, 0o755);

const kb = (statSync(dst).size / 1024 / 1024).toFixed(1);
console.log(`已备好命令行二进制：src-tauri/bin/${exe}（${profile}，${kb} MB）`);
