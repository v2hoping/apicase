// 路径工具单测：文件树的克隆 / 复制 / 剪切 / 粘贴全靠这几个纯函数算目标路径与新名字。
import { loadModule, eq, ok, report } from "./harness.mjs";

const { baseName, dirName, joinPath, relPath, isUnder, retargetPath, splitExt, uniqueName } = await loadModule("src/pathutil.ts");

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

report();
