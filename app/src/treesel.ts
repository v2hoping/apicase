// 文件树多选的纯逻辑（无 React、无后端依赖，便于单测）。
//
// 多选出问题的地方几乎都不在渲染上，而在这几个算法里：Shift 的区间取错、
// 父子同选导致「删完父再删子」、同批次重名算出两个一样的新名字。
// 故全部抽在这里，由 `test/treesel.test.mjs` 钉住。

import { baseName, checkMove, dirName, isUnder, joinPath, uniqueName } from "./pathutil";

/** 树里的一项：路径 + 是不是目录。选区、剪贴板、拖拽源都用它。 */
export interface Sel {
  path: string;
  isDir: boolean;
}

/**
 * 当前**可见**的行，按渲染顺序拉平（不含工作空间根那一行——根不参与多选）。
 *
 * Shift 的区间必须按这个顺序算，而不是按目录结构：展开的子项算在内、折叠的不算。
 * 树是递归渲染的，没有这份扁平数组就无从取"从锚点到这一行"。
 */
export function flattenVisible(
  root: string,
  childrenMap: Record<string, Sel[]>,
  expanded: Set<string>,
): Sel[] {
  const out: Sel[] = [];
  const walk = (dir: string) => {
    for (const c of childrenMap[dir] || []) {
      out.push({ path: c.path, isDir: c.isDir });
      if (c.isDir && expanded.has(c.path)) walk(c.path);
    }
  };
  walk(root);
  return out;
}

/**
 * 锚点到目标行之间的可见区间（含两端）。
 *
 * **覆盖式**：连点两次 Shift 要基于同一锚点重算，而不是在上次结果上追加——
 * 累加的实现表现为「Shift 点回去反而选得更多」。
 * 锚点已不可见（父目录被折叠 / 文件被删）时退化成只选目标行本身。
 */
export function rangeBetween(rows: Sel[], anchor: string, to: string): Sel[] {
  const a = rows.findIndex((r) => r.path === anchor);
  const b = rows.findIndex((r) => r.path === to);
  if (b < 0) return [];
  if (a < 0) return [rows[b]];
  const [from, end] = a <= b ? [a, b] : [b, a];
  return rows.slice(from, end + 1);
}

/**
 * Cmd/Ctrl 点击：在选区里加入或去掉一项，**保持可见顺序**。
 *
 * 顺序要紧：删除确认里的项数与措辞、粘贴落地的次序都按它走，
 * 按点击先后排会让同一批选中项每次的顺序都不一样。
 */
export function toggleSel(sel: Sel[], item: Sel, rows: Sel[]): Sel[] {
  if (sel.some((s) => s.path === item.path)) return sel.filter((s) => s.path !== item.path);
  const order = new Map(rows.map((r, i) => [r.path, i]));
  // 不在可见行里的（理论上不该发生）排到最后，至少不丢
  const rank = (p: string) => order.get(p) ?? Number.MAX_SAFE_INTEGER;
  return [...sel, item].sort((x, y) => rank(x.path) - rank(y.path));
}

/**
 * 祖先吸收：选区里同时有 `订单/` 和 `订单/下单.yml` 时只留最上层的那些。
 *
 * 删除、移动、复制、克隆都要先过这一道。不做的话：删除会「删完父再删子」报错，
 * 移动会在父目录移走之后拿着一个已经不存在的子路径去 rename。
 */
export function pruneDescendants(items: Sel[]): Sel[] {
  const dirs = items.filter((i) => i.isDir).map((i) => i.path);
  return items.filter((i) => !dirs.some((d) => d !== i.path && isUnder(d, i.path)));
}

/**
 * 批次内互撞的名字（两个不同目录下的同名项要移进同一个目录）。返回第一个撞上的名字，没有则 `null`。
 *
 * 单独判一次而不是等文件系统报错：`rename` 到已存在的路径会失败，
 * 但那时第一项已经移过去了，用户得自己核对哪些成功了。
 */
export function dupName(items: Sel[]): string | null {
  const seen = new Set<string>();
  for (const it of items) {
    const n = baseName(it.path);
    if (seen.has(n)) return n;
    seen.add(n);
  }
  return null;
}

/** 选区的构成，用于「删除 5 项，其中 2 个文件夹」这类文案。 */
export function countKinds(items: Sel[]): { files: number; dirs: number } {
  return {
    files: items.filter((i) => !i.isDir).length,
    dirs: items.filter((i) => i.isDir).length,
  };
}

/**
 * 一批项落进 `targetDir` 后各自的目标路径，**同批次内累积占用名**。
 *
 * 一次粘两个同名文件时，第二个必须看得见第一个刚落地的名字，
 * 否则两个都算出同一个「x 副本」，第二次拷贝直接覆盖第一次的结果。
 */
export function planCopy(items: Sel[], targetDir: string, taken: Set<string>): { from: string; to: string }[] {
  const used = new Set(taken);
  return items.map((it) => {
    const name = uniqueName(baseName(it.path), (n) => used.has(n));
    used.add(name);
    return { from: it.path, to: joinPath(targetDir, name) };
  });
}

/**
 * 一次移动（拖放）的计划：要么给出全部 `rename` 动作，要么给出一句可直接展示的错误。
 *
 * **全或无**。移了一半再报错，用户得自己逐个核对哪些已经过去了——而这正是他刚才
 * 一次拖过来是为了省掉的事。故先把整批检查完，任一冲突就整批不动。
 *
 * 单选的拖放走的是同一个函数（`items` 长度为 1），错误文案因此只有一份，
 * 不会出现「拖一个」与「拖三个」说法不同的情况。
 */
export type MovePlan = { ok: true; moves: { from: string; to: string; isDir: boolean }[] } | { ok: false; error: string };

/**
 * `dragover` 阶段能不能收下这一批（只判结构性问题：拖回原处、放到自己身上、目录进自己的子目录）。
 *
 * **同名不在这里判**：那要读目标目录，而 `dragover` 每移动几像素就触发一次。
 * 同名留到松手后报错——这也是单选时一直以来的行为（禁止放置只会得到一个没有解释的禁止图标）。
 */
export function canDropInto(items: Sel[], targetDir: string): boolean {
  return pruneDescendants(items).some((it) => checkMove(it.path, it.isDir, targetDir) === "ok");
}

export function planMove(items: Sel[], targetDir: string, taken: Set<string>): MovePlan {
  const movable: Sel[] = [];
  for (const it of pruneDescendants(items)) {
    const verdict = checkMove(it.path, it.isDir, targetDir);
    // 拖回原处 / 放到自己身上：用户什么也没要求，不是错误，跳过即可
    if (verdict === "noop" || verdict === "self") continue;
    if (verdict === "into-self") return { ok: false, error: "不能把文件夹移动到它自己或它的子目录中" };
    movable.push(it);
  }
  const dup = dupName(movable);
  if (dup) return { ok: false, error: `选中项里有两个 ${dup}，不能一起移动到 ${baseName(targetDir)}` };

  // 同名一律拒绝、不自动排号（与「粘贴」刻意不同）：移动时悄悄改名，
  // 会让人在新位置找不到自己刚拖过去的东西
  const hit = movable.filter((it) => taken.has(baseName(it.path))).map((it) => baseName(it.path));
  if (hit.length) {
    const tail = hit.length > 1 ? ` 等 ${hit.length} 项` : "";
    return { ok: false, error: `${baseName(targetDir)} 下已有 ${hit[0]}${tail}` };
  }
  return { ok: true, moves: movable.map((it) => ({ from: it.path, to: joinPath(targetDir, baseName(it.path)), isDir: it.isDir })) };
}

/** 同上，但目标是各自所在的目录（克隆）。`takenOf` 给出某目录当前已占用的名字。 */
export function planClone(items: Sel[], takenOf: (dir: string) => Set<string>): { from: string; to: string }[] {
  const usedByDir = new Map<string, Set<string>>();
  return items.map((it) => {
    const dir = dirName(it.path);
    if (!usedByDir.has(dir)) usedByDir.set(dir, new Set(takenOf(dir)));
    const used = usedByDir.get(dir)!;
    const name = uniqueName(baseName(it.path), (n) => used.has(n));
    used.add(name);
    return { from: it.path, to: joinPath(dir, name) };
  });
}
