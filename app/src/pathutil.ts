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
