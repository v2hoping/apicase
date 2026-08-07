// 文件树多选的纯逻辑单测。多选做砸的地方几乎都在这里：
// Shift 区间取错、父子同选导致「删完父再删子」、同批次重名算出两个一样的新名字。
import { loadModule, eq, ok, report } from "./harness.mjs";

const {
  flattenVisible,
  rangeBetween,
  toggleSel,
  pruneDescendants,
  dupName,
  countKinds,
  planCopy,
  planClone,
  planMove,
  canDropInto,
} = await loadModule("src/treesel.ts");

// 一棵测试树（/ws 下）：
//   订单/            ← 目录
//     下单.yml
//     退款.yml
//   用户/            ← 目录（折叠时其子项不可见）
//     登录.yml
//   hello.yml
const f = (p) => ({ path: p, isDir: false });
const d = (p) => ({ path: p, isDir: true });
const CHILDREN = {
  "/ws": [d("/ws/订单"), d("/ws/用户"), f("/ws/hello.yml")],
  "/ws/订单": [f("/ws/订单/下单.yml"), f("/ws/订单/退款.yml")],
  "/ws/用户": [f("/ws/用户/登录.yml")],
};

// ── flattenVisible：Shift 区间的地基 ──
eq(
  flattenVisible("/ws", CHILDREN, new Set()).map((r) => r.path),
  ["/ws/订单", "/ws/用户", "/ws/hello.yml"],
  "全折叠时只有顶层三行",
);
eq(
  flattenVisible("/ws", CHILDREN, new Set(["/ws/订单"])).map((r) => r.path),
  ["/ws/订单", "/ws/订单/下单.yml", "/ws/订单/退款.yml", "/ws/用户", "/ws/hello.yml"],
  "展开的目录，子项按渲染顺序插在它后面",
);
eq(flattenVisible("/ws", {}, new Set()).length, 0, "还没加载过子项时是空的，不报错");
// 根不在里面：它不能被删/移/复制，也就不该参与多选
ok(
  !flattenVisible("/ws", CHILDREN, new Set()).some((r) => r.path === "/ws"),
  "工作空间根不参与多选",
);

const ROWS = flattenVisible("/ws", CHILDREN, new Set(["/ws/订单"]));

// ── rangeBetween：覆盖式，不累加 ──
eq(
  rangeBetween(ROWS, "/ws/订单/下单.yml", "/ws/用户").map((r) => r.path),
  ["/ws/订单/下单.yml", "/ws/订单/退款.yml", "/ws/用户"],
  "锚点到目标的连续区间（含两端）",
);
eq(
  rangeBetween(ROWS, "/ws/用户", "/ws/订单/下单.yml").map((r) => r.path),
  ["/ws/订单/下单.yml", "/ws/订单/退款.yml", "/ws/用户"],
  "反向选也是同一段（顺序恒按可见顺序）",
);
eq(rangeBetween(ROWS, "/ws/hello.yml", "/ws/hello.yml").map((r) => r.path), ["/ws/hello.yml"], "同一行即它自己");
// 连点两次 Shift：第二次基于同一锚点重算，不是在上次结果上追加
eq(
  rangeBetween(ROWS, "/ws/订单", "/ws/订单/退款.yml").map((r) => r.path).length,
  3,
  "Shift 连点第二次仍从锚点算起",
);
eq(
  rangeBetween(ROWS, "/ws/订单", "/ws/订单").map((r) => r.path),
  ["/ws/订单"],
  "Shift 点回锚点自身只剩一项（累加实现会越选越多）",
);
eq(
  rangeBetween(ROWS, "/ws/用户/登录.yml", "/ws/hello.yml").map((r) => r.path),
  ["/ws/hello.yml"],
  "锚点已不可见（父目录被折叠）时退化成只选目标行",
);
eq(rangeBetween(ROWS, "/ws/订单", "/ws/不存在.yml"), [], "目标行不存在则不改选区");

// ── toggleSel：Cmd 点击 ──
eq(
  toggleSel([f("/ws/hello.yml")], f("/ws/订单/下单.yml"), ROWS).map((r) => r.path),
  ["/ws/订单/下单.yml", "/ws/hello.yml"],
  "加入后按可见顺序排，而不是点击先后",
);
eq(
  toggleSel([f("/ws/hello.yml"), f("/ws/订单/下单.yml")], f("/ws/hello.yml"), ROWS).map((r) => r.path),
  ["/ws/订单/下单.yml"],
  "点已选中的项 = 取消它",
);
eq(toggleSel([f("/ws/hello.yml")], f("/ws/hello.yml"), ROWS), [], "取消到空");

// ── pruneDescendants：祖先吸收 ──
eq(
  pruneDescendants([d("/ws/订单"), f("/ws/订单/下单.yml"), f("/ws/hello.yml")]).map((r) => r.path),
  ["/ws/订单", "/ws/hello.yml"],
  "选了父目录就忽略其下的子项",
);
eq(
  pruneDescendants([f("/ws/订单/下单.yml"), f("/ws/订单/退款.yml")]).map((r) => r.path),
  ["/ws/订单/下单.yml", "/ws/订单/退款.yml"],
  "没选父目录时两个子项都留着",
);
eq(pruneDescendants([d("/ws/订单")]).length, 1, "自己不吸收自己");
// 同名前缀的兄弟目录不算子目录（/ws/订单2 不在 /ws/订单 之下）
eq(pruneDescendants([d("/ws/订单"), d("/ws/订单2")]).length, 2, "同名前缀的兄弟目录各自保留");

// ── dupName / countKinds ──
eq(dupName([f("/a/x.yml"), f("/b/x.yml")]), "x.yml", "不同目录下的同名项撞上了");
eq(dupName([f("/a/x.yml"), f("/a/y.yml")]), null, "不重名时为 null");
eq(countKinds([d("/a"), f("/a2.yml"), f("/a3.yml")]), { files: 2, dirs: 1 }, "文件与目录各几个");

// ── planCopy：粘贴，批次内累积占用名 ──
eq(
  planCopy([f("/a/x.yml"), f("/b/x.yml")], "/dst", new Set()),
  [
    { from: "/a/x.yml", to: "/dst/x.yml" },
    { from: "/b/x.yml", to: "/dst/x 副本.yml" },
  ],
  "一次粘两个同名文件：第二个看得见第一个刚落地的名字",
);
eq(
  planCopy([f("/a/x.yml")], "/dst", new Set(["x.yml"])),
  [{ from: "/a/x.yml", to: "/dst/x 副本.yml" }],
  "目标目录已有同名则排号",
);

// ── planClone：克隆到各自所在目录 ──
eq(
  planClone([f("/a/x.yml"), f("/a/y.yml")], () => new Set(["x.yml", "y.yml"])),
  [
    { from: "/a/x.yml", to: "/a/x 副本.yml" },
    { from: "/a/y.yml", to: "/a/y 副本.yml" },
  ],
  "各自在所在目录排号",
);
eq(
  planClone([f("/a/x.yml"), f("/b/x.yml")], () => new Set(["x.yml"])),
  [
    { from: "/a/x.yml", to: "/a/x 副本.yml" },
    { from: "/b/x.yml", to: "/b/x 副本.yml" },
  ],
  "不同目录各算各的，互不影响",
);

// ── planMove：全或无 + 文案与单选一致 ──
const okMove = planMove([f("/ws/a.yml"), f("/ws/b.yml")], "/ws/订单", new Set());
eq(okMove.ok, true, "无冲突时给出计划");
eq(
  okMove.moves,
  [
    { from: "/ws/a.yml", to: "/ws/订单/a.yml", isDir: false },
    { from: "/ws/b.yml", to: "/ws/订单/b.yml", isDir: false },
  ],
  "移动不排号（与粘贴刻意不同：悄悄改名会让人在新位置找不到东西）",
);

// 目标目录已有同名：文案与单选时逐字一致
eq(
  planMove([f("/ws/a.yml")], "/ws/订单", new Set(["a.yml"])),
  { ok: false, error: "订单 下已有 a.yml" },
  "单个同名的提示与改造前一致",
);
eq(
  planMove([f("/ws/a.yml"), f("/ws/b.yml")], "/ws/订单", new Set(["a.yml", "b.yml"])),
  { ok: false, error: "订单 下已有 a.yml 等 2 项" },
  "多个同名时前半句不变，只补「等 N 项」",
);
// 批次内互撞：两个不同目录下的同名项要进同一个目录
eq(
  planMove([f("/ws/x/a.yml"), f("/ws/y/a.yml")], "/ws/订单", new Set()),
  { ok: false, error: "选中项里有两个 a.yml，不能一起移动到 订单" },
  "拖拽中同名也报错，且整批不动",
);
// 结构性拒绝：目录进自己的子目录
eq(
  planMove([d("/ws/订单")], "/ws/订单/子", new Set()),
  { ok: false, error: "不能把文件夹移动到它自己或它的子目录中" },
  "文件夹不能进自己的子目录（现有文案）",
);
// 拖回原处：不是错误，静默跳过
const noop = planMove([f("/ws/订单/下单.yml")], "/ws/订单", new Set());
eq(noop, { ok: true, moves: [] }, "拖回原处 = 什么也不做，不弹错误");
// 一半原地一半要移：只移该移的那些
const partial = planMove([f("/ws/订单/下单.yml"), f("/ws/hello.yml")], "/ws/订单", new Set());
eq(partial.ok && partial.moves.map((m) => m.from), ["/ws/hello.yml"], "已在目标目录里的那项跳过");
// 父子同选：只移父目录，子项跟着走（否则父移走后子的源路径已不存在）
const withChild = planMove([d("/ws/订单"), f("/ws/订单/下单.yml")], "/ws/用户", new Set());
eq(withChild.ok && withChild.moves.map((m) => m.from), ["/ws/订单"], "父子同选只移父目录");

// ── canDropInto：dragover 阶段只判结构性问题 ──
ok(canDropInto([f("/ws/hello.yml")], "/ws/订单"), "能放");
ok(!canDropInto([f("/ws/订单/下单.yml")], "/ws/订单"), "已经在里面了，不给放置反馈");
ok(!canDropInto([d("/ws/订单")], "/ws/订单/子"), "目录不能放进自己的子目录");
ok(canDropInto([f("/ws/订单/下单.yml"), f("/ws/hello.yml")], "/ws/订单"), "一批里只要有一项能移就收下");
// 同名不在这里判——它要读盘，而 dragover 每移动几像素就触发一次
ok(canDropInto([f("/ws/x/a.yml"), f("/ws/y/a.yml")], "/ws/订单"), "批次内同名留到松手后报错");

report();
