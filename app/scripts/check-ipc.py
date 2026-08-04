"""核对前端 invoke 调用与 Rust 命令签名是否一致。

参数名一旦对不上，表现是运行时 invoke reject——而那既没有编译错误、
也没有类型错误，只有点下去才发现。故用静态核对补上这一环。
"""
import re, pathlib, sys

def balanced(src, i, quotes='"\'`'):
    """从 src[i]（一个开括号）起返回匹配的闭括号下标。会跳过字符串与模板串。

    `quotes` 可调：扫 Rust 时要去掉单引号，否则生命周期 `'_` 会被当成字符串起始，
    一路吞到下一个单引号为止。
    """
    pairs = {'(': ')', '{': '}', '[': ']'}
    stack = [pairs[src[i]]]
    i += 1
    while i < len(src) and stack:
        c = src[i]
        if c in quotes:
            q = c; i += 1
            while i < len(src) and src[i] != q:
                i += 2 if src[i] == '\\' else 1
        elif c in pairs:
            stack.append(pairs[c])
        elif stack and c == stack[-1]:
            stack.pop()
        i += 1
    return i - 1

def top_keys(obj):
    """取对象字面量的顶层键名（含简写属性）。"""
    body, keys, i, depth = obj[1:-1], [], 0, 0
    start = 0
    parts = []
    while i < len(body):
        c = body[i]
        if c in '"\'`':
            q = c; i += 1
            while i < len(body) and body[i] != q:
                i += 2 if body[i] == '\\' else 1
        elif c in '({[':
            depth += 1
        elif c in ')}]':
            depth -= 1
        elif c == ',' and depth == 0:
            parts.append(body[start:i]); start = i + 1
        i += 1
    parts.append(body[start:])
    for p in parts:
        p = p.strip()
        if not p: continue
        m = re.match(r'^\.\.\.', p)
        if m: keys.append('...'); continue
        m = re.match(r'^["\']?([A-Za-z_$][\w$]*)["\']?\s*(:|$)', p)
        if m: keys.append(m.group(1))
    return sorted(keys)

# ── Rust 侧 ────────────────────────────────────────
cmds = {}
for f in pathlib.Path('src-tauri/src/commands').glob('*.rs'):
    src = f.read_text()
    for m in re.finditer(r'#\[tauri::command\]\s*\n\s*pub (?:async )?fn (\w+)\s*\(', src):
        name = m.group(1)
        open_i = src.index('(', m.end() - 1)
        params = src[open_i + 1: balanced(src, open_i, quotes='"')]
        args, depth, start = [], 0, 0
        segs = []
        for i, c in enumerate(params):
            if c in '<([{': depth += 1
            elif c in '>)]}': depth -= 1
            elif c == ',' and depth == 0:
                segs.append(params[start:i]); start = i + 1
        segs.append(params[start:])
        for p in segs:
            p = p.strip()
            if not p or ':' not in p: continue
            pname, ptype = p.split(':', 1)
            if 'AppHandle' in ptype or 'State<' in ptype: continue
            camel = re.sub(r'_(\w)', lambda x: x.group(1).upper(), pname.strip())
            # Option<T> 的参数前端可以不传（Tauri 会填 None）
            args.append((camel, ptype.strip().startswith('Option<')))
        cmds[name] = args

# ── 前端侧 ─────────────────────────────────────────
calls = []
for f in sorted(list(pathlib.Path('src').glob('*.ts')) + list(pathlib.Path('src').glob('*.tsx'))):
    src = f.read_text()
    for m in re.finditer(r'\binvoke\s*(?:<[^(]*?>)?\s*\(', src):
        open_i = src.index('(', m.end() - 1)
        inner = src[open_i + 1: balanced(src, open_i)]
        cm = re.match(r'\s*"([^"]+)"\s*(?:,\s*)?', inner)
        if not cm: continue
        rest = inner[cm.end():].strip()
        keys = top_keys(rest) if rest.startswith('{') else []
        calls.append((f.name, cm.group(1), keys))

bad = []
print("=== 前端 invoke → Rust 命令 核对 ===\n")
for path, name, keys in sorted(calls, key=lambda c: (c[1], c[0])):
    if name not in cmds:
        print(f"  ✗ {name:20} 命令不存在                       {path}"); bad.append(name); continue
    required = sorted(a for a, opt in cmds[name] if not opt)
    allowed = sorted(a for a, _ in cmds[name])
    if '...' in keys:
        print(f"  ~ {name:20} 含展开运算符，跳过精确核对        {path}"); continue
    missing = [a for a in required if a not in keys]
    extra = [k for k in keys if k not in allowed]
    if missing or extra:
        detail = (f"缺 {missing} " if missing else "") + (f"多 {extra}" if extra else "")
        print(f"  ✗ {name:20} {detail}（签名 {allowed}）   {path}"); bad.append(name)
    else:
        opt_note = "".join(f" +可选{a}" for a, o in cmds[name] if o and a not in keys)
        print(f"  ✓ {name:20} {keys if keys else '（无参数）'}{opt_note}")

registered = set(re.findall(r'\b(?:exec|app|cookies|fs|watch|terminal)::(\w+),', pathlib.Path('src-tauri/src/lib.rs').read_text()))
called = {c[1] for c in calls}
print(f"\n定义了 {len(cmds)} 个命令，注册 {len(registered)} 个，前端调用 {len(called)} 个")
missing_reg = sorted(set(cmds) - registered)
never_called = sorted(registered - called)
if missing_reg: print(f"  ⚠ 定义但未注册（前端调不到）: {missing_reg}")
if never_called: print(f"  · 注册但前端未调用: {never_called}")
print(f"\n{'✗ 有 ' + str(len(bad)) + ' 处接线不一致: ' + str(sorted(set(bad))) if bad else '✓ 全部 IPC 接线一致'}")
sys.exit(1 if bad or missing_reg else 0)
