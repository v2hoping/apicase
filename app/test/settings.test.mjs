// 工作空间请求设置（application.yml 的 settings: 键）单测。
// 这份配置直接决定「是否校验证书 / 用哪张 CA / 超时多久」，解析容错与落盘裁剪都得可靠：
// 手写配置写错一个键不该让请求功能瘫掉，全默认时也不该往用户的配置文件里塞噪声。
import { loadModule, eq, ok, has, hasnt, report } from "./harness.mjs";

const { parseSettings, dumpApplicationConfig, DEFAULT_WS_SETTINGS } = await loadModule("src/case.ts");

const DEFAULTS = { verifySsl: true, useCustomCa: false, caCert: "", timeoutMs: 0 };

// ── 默认值 ──
eq(DEFAULT_WS_SETTINGS, DEFAULTS, "默认：校验开启 / 无自定义 CA / 不限超时");

// ── 解析：缺失与异常输入一律回落默认（安全侧兜底）──
eq(parseSettings(""), DEFAULTS, "空文本回默认");
eq(parseSettings("environment:\n  dev: {}\n"), DEFAULTS, "没有 settings 键回默认");
eq(parseSettings("settings:\n"), DEFAULTS, "settings 为空回默认");
eq(parseSettings("settings: 不是对象\n"), DEFAULTS, "settings 类型不符回默认");
eq(parseSettings("::: 这不是 YAML :::\n"), DEFAULTS, "YAML 解析失败回默认，不抛错");
eq(parseSettings("- 顶层是列表\n"), DEFAULTS, "顶层非对象回默认");

// ── 解析：verifySsl 只有显式 false 才关闭 ──
eq(parseSettings("settings:\n  verifySsl: false\n").verifySsl, false, "显式 false 才关闭校验");
eq(parseSettings("settings:\n  verifySsl: true\n").verifySsl, true, "显式 true 保持开启");
eq(parseSettings("settings:\n  verifySsl: 0\n").verifySsl, true, "写成 0（非布尔）按开启兜底");
eq(parseSettings('settings:\n  verifySsl: "false"\n').verifySsl, true, "写成字符串 false 也按开启兜底");

// ── 解析：useCustomCa 只有显式 true 才启用 ──
eq(parseSettings("settings:\n  useCustomCa: true\n").useCustomCa, true, "显式 true 启用自定义 CA");
eq(parseSettings('settings:\n  useCustomCa: "true"\n').useCustomCa, false, "字符串 true 不算启用");

// ── 解析：caCert 去空白、非字符串归空 ──
eq(parseSettings("settings:\n  caCert: '  certs/ca.crt  '\n").caCert, "certs/ca.crt", "CA 路径去两端空白");
eq(parseSettings("settings:\n  caCert: 123\n").caCert, "", "CA 路径非字符串归空");

// ── 解析：timeoutMs 的钳制 ──
eq(parseSettings("settings:\n  timeoutMs: 30000\n").timeoutMs, 30000, "正常超时值");
eq(parseSettings("settings:\n  timeoutMs: 0\n").timeoutMs, 0, "0 = 不限制");
eq(parseSettings("settings:\n  timeoutMs: -5\n").timeoutMs, 0, "负数归 0");
eq(parseSettings("settings:\n  timeoutMs: 1500.9\n").timeoutMs, 1500, "小数向下取整");
eq(parseSettings("settings:\n  timeoutMs: 慢\n").timeoutMs, 0, "非数字归 0");
eq(parseSettings('settings:\n  timeoutMs: "8000"\n').timeoutMs, 8000, "数字字符串可转换");

// ── 序列化：裁剪默认值 ──
const base = "environment:\n  dev: { baseUrl: http://x }\n";
const envs = { dev: { baseUrl: "http://x" } };

const allDefault = dumpApplicationConfig(base, envs, DEFAULTS);
hasnt(allDefault, "settings", "全默认时整个 settings 键不落盘");
has(allDefault, "environment", "environment 照常落盘");

const partial = dumpApplicationConfig(base, envs, { ...DEFAULTS, timeoutMs: 30000 });
has(partial, "timeoutMs: 30000", "非默认的超时落盘");
hasnt(partial, "verifySsl", "仍为默认的 verifySsl 不落盘");
hasnt(partial, "useCustomCa", "仍为默认的 useCustomCa 不落盘");

const full = dumpApplicationConfig(base, envs, {
  verifySsl: false,
  useCustomCa: true,
  caCert: "certs/ca.crt",
  timeoutMs: 5000,
});
has(full, "verifySsl: false", "关闭校验落盘");
has(full, "useCustomCa: true", "启用自定义 CA 落盘");
has(full, "certs/ca.crt", "CA 路径落盘");
has(full, "timeoutMs: 5000", "超时落盘");

// 空白 CA 路径视同未配置
hasnt(dumpApplicationConfig(base, envs, { ...DEFAULTS, caCert: "   " }), "caCert", "纯空白的 CA 路径不落盘");

// ── 序列化：省略第三参时不动原文的 settings（兼容既有调用）──
const withSettings = "settings:\n  timeoutMs: 9000\n" + base;
has(dumpApplicationConfig(withSettings, envs), "timeoutMs: 9000", "不传 settings 时保留原有的键");
// 从有值改回全默认 → 整键删除，不留空壳
hasnt(dumpApplicationConfig(withSettings, envs, DEFAULTS), "settings", "改回全默认时删除 settings 键");

// ── 往返：dump → parse 得回原值 ──
const round = {
  verifySsl: false,
  useCustomCa: true,
  caCert: "certs/corp-ca.pem",
  timeoutMs: 12000,
};
eq(parseSettings(dumpApplicationConfig(base, envs, round)), round, "dump → parse 往返一致");

// 与 environment 互不干扰
const both = dumpApplicationConfig(base, { prod: { baseUrl: "http://p" } }, round);
eq(parseSettings(both), round, "写入 settings 不影响其解析");
ok(both.includes("prod"), "写入 settings 的同时 environment 照常更新");

// 其它顶层键要保留（用户可能手写了别的配置）
const extra = "custom: keep-me\n" + base;
has(dumpApplicationConfig(extra, envs, round), "keep-me", "保留无关的顶层键");

// settings 里的未知键同样要保留——一次可视化保存不该吃掉用户手写的内容
const withUnknown = "settings:\n  timeoutMs: 100\n  retries: 3\n" + base;
const kept = dumpApplicationConfig(withUnknown, envs, { ...DEFAULTS, timeoutMs: 7000 });
has(kept, "retries: 3", "settings 里的未知键保留");
has(kept, "timeoutMs: 7000", "已知键按新值覆盖");
// 未知键存在时，即使已知项全为默认也不能删掉整个 settings 键
const keptAllDefault = dumpApplicationConfig(withUnknown, envs, DEFAULTS);
has(keptAllDefault, "retries: 3", "已知项全默认时仍保留未知键");
hasnt(keptAllDefault, "timeoutMs", "已知项回默认则从文件中移除");

report();
