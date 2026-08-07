// 路径工具单测：文件树的克隆 / 复制 / 剪切 / 粘贴全靠这几个纯函数算目标路径与新名字。
import { loadModule, eq, ok, report } from "./harness.mjs";

const {
  baseName,
  dirName,
  joinPath,
  relPath,
  isUnder,
  retargetPath,
  splitExt,
  uniqueName,
  reportFileName,
  resolveInWorkspace,
  dropTargetDir,
  checkMove,
  reportFileNameMulti,
} = await loadModule("src/pathutil.ts");

// ── baseName / dirName / joinPath ──
eq(baseName("/a/b/c.yml"), "c.yml", "POSIX 路径取最后一段");
eq(baseName("C:\\a\\b.yml"), "b.yml", "Windows 路径取最后一段");
eq(baseName("/a/b/"), "b", "忽略结尾分隔符");
eq(baseName("  /a/b.yml  "), "b.yml", "去两端空白");
eq(baseName("solo"), "solo", "无分隔符原样返回");

eq(dirName("/a/b/c.yml"), "/a/b", "取父目录");
eq(dirName("C:\\a\\b.yml"), "C:\\a", "Windows 取父目录");
eq(dirName("/a"), "/a", "分隔符在首位时原样返回");

eq(joinPath("/a/b", "c.yml"), "/a/b/c.yml", "POSIX 拼接");
eq(joinPath("/a/b/", "c.yml"), "/a/b/c.yml", "已带分隔符不重复");
eq(joinPath("C:\\a", "c.yml"), "C:\\a\\c.yml", "分隔符跟随目录风格");

eq(relPath("/root", "/root/a/b"), "a/b", "相对工作空间根");
eq(relPath("/root", "/root"), "root", "根自身回退为名称");
eq(relPath("/root", "/other/x"), "/other/x", "不在根下则原样");

// ── isUnder：粘贴到自身子目录的拦截依据 ──
ok(isUnder("/a", "/a"), "自身算 under");
ok(isUnder("/a", "/a/b/c"), "子孙算 under");
ok(isUnder("C:\\a", "C:\\a\\b"), "Windows 分隔符同样识别");
ok(!isUnder("/a", "/ab"), "同前缀但不同目录不算 under");
ok(!isUnder("/a/b", "/a"), "父目录不算 under");

// ── retargetPath：移动 / 重命名后标签与树状态的路径迁移 ──
eq(retargetPath("/a/x.yml", "/a", "/b"), "/b/x.yml", "目录移动带动其下文件");
eq(retargetPath("/a", "/a", "/b"), "/b", "路径自身被改写");
eq(retargetPath("/other/x.yml", "/a", "/b"), "/other/x.yml", "无关路径原样");
eq(retargetPath("/ab/x.yml", "/a", "/b"), "/ab/x.yml", "同前缀的兄弟目录不受影响");

// ── splitExt / uniqueName ──
eq(splitExt("a.yml"), { stem: "a", ext: ".yml" }, "拆主干与扩展名");
eq(splitExt("a.b.yml"), { stem: "a.b", ext: ".yml" }, "按最后一个点拆");
eq(splitExt("folder"), { stem: "folder", ext: "" }, "无扩展名");
eq(splitExt(".gitignore"), { stem: ".gitignore", ext: "" }, "开头的点不算扩展名");

{
  const taken = new Set(["用例.yml"]);
  eq(uniqueName("新的.yml", (n) => taken.has(n)), "新的.yml", "不重名时原样返回");
  eq(uniqueName("用例.yml", (n) => taken.has(n)), "用例 副本.yml", "重名 → 副本");

  taken.add("用例 副本.yml");
  eq(uniqueName("用例.yml", (n) => taken.has(n)), "用例 副本 2.yml", "副本也被占 → 副本 2");

  taken.add("用例 副本 2.yml");
  eq(uniqueName("用例.yml", (n) => taken.has(n)), "用例 副本 3.yml", "继续排号");
  // 克隆一个副本时从原始主干继续排号，不滚成「副本 副本」
  eq(uniqueName("用例 副本.yml", (n) => taken.has(n)), "用例 副本 3.yml", "克隆副本不产生「副本 副本」");
}
{
  const taken = new Set(["接口", "接口 副本"]);
  eq(uniqueName("接口", (n) => taken.has(n)), "接口 副本 2", "目录（无扩展名）同样排号");
}

// ── 报告页给的用例路径 → 工作空间内的绝对路径 ──
// 这里出过一个真实的 bug：`isUnder(parent, p)` 两个参数同型，调用时写成了 isUnder(abs, workspace)
// ——问的是"工作空间在不在这个 .yml 之下"，恒为 false，于是「在 apicase 中打开」点了永远没反应，
// 且不报任何错。故把判断收进这一个函数，用例正着反着都钉一遍。

const WS = "/Users/me/apitest";
eq(resolveInWorkspace(WS, WS, "01-方法/get.yml"), "/Users/me/apitest/01-方法/get.yml", "相对路径解析到工作空间内");
eq(resolveInWorkspace(WS, "", "a.yml"), "/Users/me/apitest/a.yml", "报告没记工作空间根时退回当前工作空间");
ok(isUnder(WS, resolveInWorkspace(WS, WS, "a/b.yml")), "结果必须落在工作空间内（参数顺序回归）");

// 报告会被转发，file 字段是不可信输入
eq(resolveInWorkspace(WS, WS, "../../etc/passwd"), null, "含 .. 的路径拒绝——仅靠前缀检查挡不住它");
eq(resolveInWorkspace(WS, WS, "a/../../../x.yml"), null, ".. 藏在中间同样拒绝");
eq(resolveInWorkspace(WS, WS, "/etc/passwd"), null, "绝对路径拒绝");
eq(resolveInWorkspace(WS, WS, "C:\\Windows\\x.yml"), null, "带盘符的绝对路径拒绝");
eq(resolveInWorkspace(WS, "/other/workspace", "a.yml"), null, "报告来自别的工作空间：不打开");
eq(resolveInWorkspace("", WS, "a.yml"), null, "没有打开工作空间时不打开");
eq(resolveInWorkspace(WS, WS, ""), null, "空文件名不打开");
eq(resolveInWorkspace(WS, WS, "..x/a.yml"), "/Users/me/apitest/..x/a.yml", "只拒绝完整的 .. 段，不误伤以点开头的目录名");

// ── 运行报告的文件名 ──
// 报告是自包含单文件，不套目录；名字要能一眼认出「什么时候跑的、跑了什么」。

const at = new Date(2026, 6, 28, 21, 57, 58); // 本地时间 2026-07-28 21:57:58（月份 0 起）
eq(reportFileName(at, "/ws/01-方法"), "20260728215758-01-方法.html", "目录：时间戳 + 目录名");
eq(reportFileName(at, "/ws/07-多步flow/登录取token链.yml"), "20260728215758-登录取token链.html", "单文件：去掉 .yml");
eq(reportFileName(at, "/ws/a.YAML"), "20260728215758-a.html", "扩展名大小写不敏感");
eq(reportFileName(at, "/Users/me/apitest"), "20260728215758-apitest.html", "工作空间根用它自己的目录名");

// 时间戳打头，字典序才等于时间序——排序错了就得满目录找"最近那次"
const later = reportFileName(new Date(2026, 6, 28, 22, 0, 0), "/ws/aaa");
ok(reportFileName(at, "/ws/zzz") < later, "先跑的排在前面，与目标名无关");

// 各字段补零：9 月 3 日 08:07:06 不能写成 202693876
eq(reportFileName(new Date(2026, 8, 3, 8, 7, 6), "/ws/x"), "20260903080706-x.html", "月日时分秒一律补零");

// 文件系统不收的字符（Windows 禁 \ / : * ? " < > |）换成 -，且不留连续/首尾的 -
eq(reportFileName(at, '/ws/a:b*c?d"e<f>g|h'), "20260728215758-a-b-c-d-e-f-g-h.html", "非法字符换成连字符");
eq(reportFileName(at, "/ws/--怪 名字--"), "20260728215758-怪 名字.html", "首尾的连字符与空白去掉");

// 目录名上限 255 字节，一个汉字 3 字节：截到 60 字节且不切碎汉字
{
  const name = reportFileName(at, "/ws/" + "长".repeat(50));
  const suffix = name.slice("20260728215758-".length, -".html".length);
  eq(suffix, "长".repeat(20), "按字节截断，切点落在字符边界上");
  ok(new TextEncoder().encode(suffix).length <= 60, "截断后不超过 60 字节");
}

// 净化后什么都不剩时只留时间戳——宁可少个后缀，也不能拼出建不出来的文件名
eq(reportFileName(at, "/ws/..."), "20260728215758.html", "全是点");
eq(reportFileName(at, "/ws/:::"), "20260728215758.html", "全是非法字符");

// ── 拖拽移动：落点与合法性 ──
//
// 这几条判定错一次的代价都不小：把目录搬进它自己会连数据一起丢，
// 而"拖回原处"报个错则纯属打扰。

// 落点：文件夹是它自己，文件是它所在的目录（拖到哪一行就落到哪一行所属的目录）
eq(dropTargetDir({ path: "/ws/api", isDir: true }), "/ws/api", "拖到文件夹上");
eq(dropTargetDir({ path: "/ws/api/login.yml", isDir: false }), "/ws/api", "拖到文件上＝它所在的目录");

eq(checkMove("/ws/a.yml", false, "/ws/api"), "ok", "文件挪进别的目录");
eq(checkMove("/ws/api", true, "/ws/old"), "ok", "目录挪进别的目录");

// 拖回原处不是错误，静默即可
eq(checkMove("/ws/api/login.yml", false, "/ws/api"), "noop", "已经在目标目录里");
eq(checkMove("/ws/api", true, "/ws"), "noop", "目录已经在目标目录里");
eq(checkMove("/ws/api", true, "/ws/api"), "self", "放到自己身上");

// 目录搬进它自己：文件系统层面就是把它连同内容挪进自己的子目录，必须拦
eq(checkMove("/ws/api", true, "/ws/api/v1"), "into-self", "目录放进自己的子目录");
eq(checkMove("/ws/api", true, "/ws/api/v1/deep"), "into-self", "更深一层同理");
// 同名前缀不算子目录（/ws/api2 不在 /ws/api 之下）
eq(checkMove("/ws/api", true, "/ws/api2"), "ok", "同名前缀的兄弟目录不受影响");
// 文件没有"子目录"一说：同路径前缀的目录照样能放
eq(checkMove("/ws/api.yml", false, "/ws/api.yml.bak"), "ok", "文件不做 into-self 判定");

// ── 多目标的报告名 ──
//
// 与 core 的 `report_file_name_multi` 逐字对齐（两处不同，报告目录里就会出现两套命名）。
// 只写首个目标会让人以为只跑了它，而文件名是回头找某次运行的唯一线索。
eq(reportFileNameMulti(at, ["api"]), "20260728215758-api.html", "单个＝原来的规则");
eq(
  reportFileNameMulti(at, ["04-认证", "07-多步flow", "hello.yml"]),
  "20260728215758-04-认证等3项.html",
  "多个＝首个 + 项数",
);
eq(reportFileNameMulti(at, ["", "api", "."]), "20260728215758-api.html", "空目标不参与计数");
eq(reportFileNameMulti(at, []), "20260728215758.html", "一个都没有就只留时间戳");

report();
