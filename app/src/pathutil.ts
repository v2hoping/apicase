// 路径工具（纯字符串处理，无 React / 无后端依赖，便于单测）。
// 同时认 / 与 \ 两种分隔符：路径来自后端 std::path，Windows 上是反斜杠。

/** 取路径最后一段（文件名 / 目录名）。 */
export function baseName(p: string): string {
  const parts = p.trim().split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] || p;
}

/** 取父目录；无分隔符或只在首位（如 "/a"）时原样返回。 */
export function dirName(p: string): string {
  const idx = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return idx <= 0 ? p : p.slice(0, idx);
}

/** 拼接目录与名称，分隔符跟随 dir 的风格。 */
export function joinPath(dir: string, name: string): string {
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.endsWith(sep) ? dir + name : dir + sep + name;
}

/** 相对工作空间根的路径（搜索结果展示用）。 */
export function relPath(root: string, p: string): string {
  if (p.startsWith(root)) {
    return p.slice(root.length).replace(/^[\\/]+/, "") || baseName(p);
  }
  return p;
}

/** p 是否就是 parent 或位于 parent 之下（用于「粘进自己的子目录」拦截、标签级联处理）。 */
export function isUnder(parent: string, p: string): boolean {
  return p === parent || p.startsWith(parent + "/") || p.startsWith(parent + "\\");
}

/** 路径前缀重写：from 移动到 to 后，p 的新位置（不在 from 之下则原样返回）。 */
export function retargetPath(p: string, from: string, to: string): string {
  if (p === from) return to;
  if (p.startsWith(from + "/") || p.startsWith(from + "\\")) return to + p.slice(from.length);
  return p;
}

/** 拆文件名的主干与扩展名；开头的点不算扩展名（`.gitignore` 整体是主干）。 */
export function splitExt(name: string): { stem: string; ext: string } {
  const i = name.lastIndexOf(".");
  if (i <= 0) return { stem: name, ext: "" };
  return { stem: name.slice(0, i), ext: name.slice(i) };
}

/**
 * 把报告页给的用例路径解析成工作空间内的绝对路径；不该打开的返回 `null`。
 *
 * **`file` 来自报告 HTML，是不可信输入**——报告会被转发，你打开的可能是别人给的文件。
 * 三道关：
 *
 * 1. 含 `..` 段的直接拒绝。仅靠"绝对路径以工作空间开头"挡不住 `ws/../../etc/passwd`；
 * 2. `file` 自己是绝对路径的拒绝（拼出来的东西已经不受工作空间约束）；
 * 3. 最后才是落在工作空间内的检查。
 *
 * 抽成函数是因为第 3 步的 `isUnder(parent, p)` 两个参数同型、写反了照样编译，
 * 而写反的后果是**永远返回 false**——按钮点了没反应，且没有任何报错（这个 bug 真发生过）。
 */
export function resolveInWorkspace(workspace: string, reportRoot: string, file: string): string | null {
  if (!workspace || !file) return null;
  const segs = file.split(/[\\/]/);
  if (segs.some((s) => s === "..")) return null;
  if (file.startsWith("/") || file.startsWith("\\") || /^[A-Za-z]:/.test(file)) return null;
  const abs = joinPath(reportRoot || workspace, file);
  return isUnder(workspace, abs) ? abs : null;
}

/**
 * 运行报告的文件名：`<YYYYMMDDHHmmss>-<目标名>.html`。
 *
 * **时间戳必须在前**：目录列表按名字排序，时间戳打头才能让字典序等于时间序；
 * 目标名放前面就成了按目标分组，"最近跑的那次"得满目录翻。
 *
 * 时间戳内部**不加分隔符**（14 位连续数字，同 Rails migration 的时间戳）：
 * 目标名自带连字符是常态（`01-方法`），时间戳里再插一个就得数着看边界；
 * 连成一串之后"数字止于何处"即时间戳止于何处。
 *
 * 目标名只为认得出跑了什么（`20260728215758-01-方法.html`），故做三件事：
 * 去掉 `.yml` 扩展名、把文件系统不收的字符换成 `-`（Windows 禁 `\ / : * ? " < > |`，
 * macOS 只禁 `/`，按严的来）、按**字节**截到 60（目录名上限 255 字节，一个汉字占 3）。
 * 净化后为空就只留时间戳——宁可少个后缀，也不能拼出一个建不出来的文件名。
 */
export function reportFileName(at: Date, target: string): string {
  const p = (n: number) => String(n).padStart(2, "0");
  const stamp =
    `${at.getFullYear()}${p(at.getMonth() + 1)}${p(at.getDate())}` +
    `${p(at.getHours())}${p(at.getMinutes())}${p(at.getSeconds())}`;
  const name = clipBytes(
    baseName(target)
      .replace(/\.ya?ml$/i, "")
      // eslint-disable-next-line no-control-regex
      .replace(/[\\/:*?"<>|\x00-\x1f]+/g, "-")
      .replace(/-{2,}/g, "-")
      .replace(/^[-.\s]+|[-.\s]+$/g, ""),
    60,
  );
  return `${stamp}${name ? `-${name}` : ""}.html`;
}

/** 按 UTF-8 字节截断，切点落在字符边界上（别把一个汉字切成半个）。 */
function clipBytes(s: string, max: number): string {
  const enc = new TextEncoder();
  if (enc.encode(s).length <= max) return s;
  let out = "";
  let used = 0;
  for (const ch of s) {
    const n = enc.encode(ch).length;
    if (used + n > max) break;
    out += ch;
    used += n;
  }
  return out;
}

/**
 * 生成不重名的名称：`用例.yml` → `用例 副本.yml` → `用例 副本 2.yml` …
 * 已是「x 副本」/「x 副本 N」的从 x 继续排号，不会滚成「x 副本 副本」。
 * taken 由调用方给（通常是目标目录已有名称的集合）。
 */
export function uniqueName(name: string, taken: (candidate: string) => boolean): string {
  if (!taken(name)) return name;
  const { stem, ext } = splitExt(name);
  const m = /^(.*?) 副本(?: (\d+))?$/.exec(stem);
  const base = m && m[1] ? m[1] : stem;
  let candidate = `${base} 副本${ext}`;
  for (let i = 2; taken(candidate); i++) candidate = `${base} 副本 ${i}${ext}`;
  return candidate;
}
