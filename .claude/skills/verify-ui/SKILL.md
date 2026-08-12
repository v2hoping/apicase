---
name: verify-ui
description: 跑起 apicase 桌面应用、或验证界面样式（App.css / 组件外观）的改动。改完 CSS 想确认「没改坏、两个主题都对」时用这个；也覆盖如何启动应用、以及为什么不要用 AppleScript 去点它。
---

# 验证 apicase 的界面改动

## 先看清楚：这个应用没法程序化驱动

- **`tauri-driver` 在 macOS 上不支持**（Tauri 官方只支持 Linux / Windows），没有 WebDriver 可用。
- **不要用 AppleScript `click at {x,y}`**。实测落点不准：一次「点文件树某行」的调用同时折叠了目录
  并弹出了顶栏的工作空间下拉。它打在用户正在用的窗口上，每试一次就多一份副作用。
- 因此：**启动应用用来肉眼确认整体没崩**，而**逐个组件的对照验证走下面的对照页**。

## 一、启动应用（确认整体渲染）

```bash
cd app && npm run tauri dev
```

**先查端口，多半已经有人在跑：**

```bash
lsof -ti:1420 | while read p; do ps -p $p -o pid=,ppid=,lstart=,command= | cut -c1-120; done
```

- 端口被占 → 开发者自己开着 dev server。**不要杀它**，它的 vite HMR 已经把你改的 CSS 推给那个窗口了，
  直接截图那个实例即可。
- 硬启会因端口冲突失败，但**失败前 cargo 已经把桌面进程拉起来了**，会留下一个 `PPID=1` 的孤儿。
  按 PPID 区分再清理，别误杀开发者的（其 PPID 指向 `tauri dev` 的 node 进程）：

```bash
ps -p <pid> -o pid=,ppid=,lstart=      # PPID=1 且启动时间是刚才 → 是自己留下的
```

**截图**（`screencapture` 权限正常，无需额外授权）：

```bash
screencapture -x -o out.png            # -x 静音
```

⚠️ 这是**全屏**截图，会拍到开发者的整个桌面。看完就删，不要留在 scratchpad 里。

## 二、组件对照页（验证具体样式）—— 首选

写一个引用**真实 `App.css`** 的静态页，把改动涉及的组件并排放进去，用 headless Chrome 渲染。
零副作用、完全可控，而且能做到操作应用做不到的两件事：**把散在各页面的同类组件摆在一起直接比**、
**一条命令切换深浅主题**。

```bash
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
"$CHROME" --headless --disable-gpu --allow-file-access-from-files --hide-scrollbars \
  --virtual-time-budget=3000 \
  --screenshot=out.png --window-size=1000,760 --force-device-scale-factor=2 \
  "file:///abs/path/harness.html"
```

页面骨架（`App.css` 用**绝对路径**，配合 `--allow-file-access-from-files`）：

```html
<!doctype html>
<meta charset="utf-8">
<link rel="stylesheet" href="/abs/path/to/app/src/App.css">
<style>body { margin:0; padding:24px; background:var(--panel); font-family:Inter,sans-serif }</style>
<!-- 把要比的组件按真实 DOM 结构抄进来，同类的并排放 -->
<div class="view-switch"><button class="vs-btn active">文本</button><button class="vs-btn">流程</button></div>
<span class="seg-radio"><button class="on">全部</button><button>仅失败</button></span>
```

**三个必须记住的点：**

1. **`--virtual-time-budget=3000` 不能省。** 浮层有 `pop-in` 入场动画，不推进虚拟时间会截到
   淡入中间帧——表现为「文字发灰、颜色不对」，看着像 bug 其实是动画没播完。
2. **深色主题**：复制一份 HTML，在 `<meta>` 后插一行即可，不必改 CSS：
   ```html
   <script>document.documentElement.dataset.theme="dark"</script>
   ```
   **两个主题都要看**——这个项目全靠 CSS 变量做主题，漏掉的硬编码色只在深色下暴露。
3. **hover / 显隐态**在静态截图里看不到。要验证它们，在对照页里用一条覆盖强制显形：
   ```css
   .demo .env-row-del, .demo .icon-btn { opacity: 1 }
   ```

## 三、改完 CSS 必跑

```bash
cd app && npm run build && npm test
```

`npm test` 末尾的 `check:ipc` 会校验 42 个命令的前后端接线；`npm run build` 走 `tsc`，
能抓到改类名时漏改的 TSX。

## 四、静态排查的几条命令

排查一致性问题（而非验证某次改动）时，这几条比逐行读 5239 行 CSS 快得多：

```bash
cd app/src
# token 是否漂出档位（字号应只有 11/12/13/14/16/22，圆角 4/6/8/999，字重 500/600/700）
grep -o "font-size: [0-9.]*px" App.css | sort -u
grep -o "border-radius: [0-9]*px;" App.css | sort -u
grep -o "font-weight: [0-9]*" App.css | sort -u

# 绕过 --accent 直接用 --blue 的（方法配色 / 状态码那类语义用法除外）
grep -n "var(--blue" App.css

# 硬编码颜色（应只剩 .theme-swatch 的固定预览色）
grep -nE "(color|background|border-color): *(#[0-9a-fA-F]{3,8}|rgba?\()" App.css

# 引用了不存在的变量（会静默走回退值，深色下不跟随——曾有 var(--warn) 这么埋了一处）
comm -13 <(grep -o '^\s*--[a-z-]*' App.css | tr -d ' ' | sort -u) \
         <(grep -o 'var(--[a-z-]*' App.css | sed 's/var(//' | sort -u)
# 已知的合法例外：--tree-depth 由 TreeNode 以内联 style 传入，本就不在 :root，报出来是正常的
```

**找逐字重复的规则块**（`.seg-radio` 那种「注释说和 X 一样、实现却不一样」的反面：真重复）：

```bash
python3 - <<'PY'
import re
from collections import defaultdict
s = open('App.css').read()
d = defaultdict(list)
for sel, body in re.findall(r'([^{}]+)\{([^{}]*)\}', s):
    n = ';'.join(sorted(p.strip() for p in body.split(';') if p.strip()))
    if len(n) > 60: d[n].append(' '.join(sel.split()))
for n, sels in d.items():
    if len(sels) > 1: print(f"[{len(sels)}处] {' | '.join(x[:60] for x in sels)}")
PY
```

注意甄别：通用 flex 容器、文本截断这类**巧合相同**不该合并，合并只会制造耦合。
值得合并的是同一设计意图的重复（段控件、拖动手柄、按键胶囊那种）。

## 五、别把合理差异当成不一致

排查时容易误判，复核过的两例：

- **图标按钮尺寸不一**（16/20/24/26/28px）是对的——它们嵌在不同行高的容器里，统一会破坏行内对齐。
- **hover 底色分 `--hover` 与 `--hover-strong` 两档**也是对的：按**所处背景**分。行内关闭 / 删除按钮
  所在的那一行 hover 时本身已是 `--hover` 底，按钮得深一档才分得出来；独立工具按钮的背景是 `--panel`，
  用 `--hover` 即可。

判断标准是**「同样的东西在不同地方长得不一样」**，而不是「值不同」。
