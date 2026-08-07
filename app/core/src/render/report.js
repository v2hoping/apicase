
// 报文体的格式化：JSON 美化与 YAML 输出。
//
// 抽成独立的一段（而不是塞进下面的主 IIFE）是为了**能被单测**——
// 报告页的 JS 跑在浏览器里、没有模块系统，测试按 #region 标记截取这一段来求值。
// YAML 只用于**展示与复制**，不参与解析回读，故规则可以比 core 的 emitter 简单；
// 但引号该加的必须加，否则复制出去的那份粘到别处就变了意思。
// #region fmt
var ApicaseFmt = (function () {
  "use strict";

  function pad(n) {
    var s = "";
    for (var i = 0; i < n; i++) s += " ";
    return s;
  }

  /** 裸写会被读成别的类型、或根本读不回来的字符串，必须加引号 */
  function needQuote(s) {
    if (s === "") return true;
    if (/^\s|\s$/.test(s)) return true; // 首尾空白会被吃掉
    if (/^[-?:,\[\]{}#&*!|>'"%@`]/.test(s)) return true; // 以指示符开头
    if (/:\s|\s#/.test(s)) return true; // 值里出现 ": " 或 " #"
    if (/^(true|false|null|yes|no|on|off|~)$/i.test(s)) return true; // 布尔 / 空值形态
    if (/^[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/.test(s)) return true; // 数字形态
    return false;
  }

  /** 单引号优先：唯一的转义是把 ' 写两遍，反斜杠原样——token 与正则里反斜杠很常见 */
  function quote(s) {
    return "'" + s.replace(/'/g, "''") + "'";
  }

  function text(s) {
    return needQuote(s) ? quote(s) : s;
  }

  /**
   * 键位置更宽松（同 core 的 emitter）：键恒按字符串取用、没有类型歧义，
   * 故 `no` / `123` 这类只在**值**位置才需要引号的，作为键裸写即可——
   * 一份响应里键名成百上千，多出来的引号全是噪声。
   */
  function keyText(k) {
    var s = String(k);
    if (s === "" || /^\s|\s$/.test(s)) return quote(s);
    if (/^[-?:,\[\]{}#&*!|>'"%@`]/.test(s) || /:\s|\s#/.test(s)) return quote(s);
    return s;
  }

  /** 多行文本走块标量：以换行结尾用 |，否则 |- */
  function block(s, ind) {
    var keep = /\n$/.test(s);
    var body = s.replace(/\n$/, "").split("\n").map(function (l) {
      return pad(ind + 2) + l;
    }).join("\n");
    return (keep ? "|" : "|-") + "\n" + body;
  }

  /** 紧跟在 `key:` 之后的部分（标量带前导空格，容器带换行 + 子行） */
  function value(v, ind) {
    if (v === null || v === undefined) return " null";
    var t = typeof v;
    if (t === "number" || t === "boolean") return " " + String(v);
    if (t === "string") return v.indexOf("\n") >= 0 ? " " + block(v, ind) : " " + text(v);
    if (Array.isArray(v)) {
      if (!v.length) return " []";
      return "\n" + v.map(function (item) { return seqItem(item, ind + 2); }).join("");
    }
    var keys = Object.keys(v);
    if (!keys.length) return " {}";
    return "\n" + keys.map(function (k) { return mapItem(k, v[k], ind + 2); }).join("");
  }

  /** 拼一行：容器的 value() 已经自带尾换行，不能再补一个 */
  function line(prefix, v, ind) {
    var out = value(v, ind);
    return prefix + out + (/\n$/.test(out) ? "" : "\n");
  }

  function mapItem(k, v, ind) {
    return line(pad(ind) + keyText(k) + ":", v, ind);
  }

  function seqItem(item, ind) {
    var isMap = item !== null && typeof item === "object" && !Array.isArray(item);
    var keys = isMap ? Object.keys(item) : [];
    if (!keys.length) return line(pad(ind) + "-", item, ind);
    // 对象的首个键跟在 "- " 后面，其余键与它左对齐（缩进 = ind + 2）
    var first = line(pad(ind) + "- " + keyText(keys[0]) + ":", item[keys[0]], ind + 2);
    var rest = keys.slice(1).map(function (k) { return mapItem(k, item[k], ind + 2); }).join("");
    return first + rest;
  }

  /** JSON 值 → YAML 文本 */
  function toYaml(v) {
    if (v === null || typeof v !== "object") return value(v, 0).slice(1) + "\n";
    if (Array.isArray(v)) {
      return v.length ? v.map(function (i) { return seqItem(i, 0); }).join("") : "[]\n";
    }
    var keys = Object.keys(v);
    return keys.length ? keys.map(function (k) { return mapItem(k, v[k], 0); }).join("") : "{}\n";
  }

  /** 文本能解析成 JSON 就返回解析结果，否则 undefined（据此决定要不要给格式切换） */
  function asJson(t) {
    if (!t) return undefined;
    try {
      var v = JSON.parse(t);
      return typeof v === "object" && v !== null ? v : undefined;
    } catch (e) {
      return undefined;
    }
  }

  /** 按格式渲染一段报文体：认不出 JSON 时一律回落原文，不糊弄 */
  function format(raw, fmt) {
    var v = asJson(raw);
    if (!v) return raw;
    if (fmt === "yaml") return toYaml(v);
    if (fmt === "raw") return raw;
    return JSON.stringify(v, null, 2); // 默认 json：缩进两格的美化形态
  }

  return { toYaml: toYaml, asJson: asJson, format: format };
})();
// #endregion fmt

(function () {
  "use strict";
  var state = { report: null, filter: "all", query: "", host: false, view: "visual" };
  var rows = new Map(); // file -> { el: <details>, sig: string }

  function esc(s) {
    return String(s == null ? "" : s)
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  }
  function fmtMs(ms) {
    if (ms == null) return "—";
    if (ms < 1000) return Math.round(ms) + "ms";
    if (ms < 60000) return (ms / 1000).toFixed(1) + "s";
    var m = Math.floor(ms / 60000), s = Math.round((ms % 60000) / 1000);
    return m + "m" + (s ? " " + s + "s" : "");
  }
  function fmtBytes(n) {
    if (!n) return "0 B";
    if (n < 1024) return n + " B";
    if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
    return (n / 1024 / 1024).toFixed(1) + " MB";
  }
  function fmtTime(iso) {
    if (!iso) return "—";
    var d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    var p = function (n) { return (n < 10 ? "0" : "") + n; };
    return d.getFullYear() + "-" + p(d.getMonth() + 1) + "-" + p(d.getDate()) + " " +
      p(d.getHours()) + ":" + p(d.getMinutes()) + ":" + p(d.getSeconds());
  }
  function statusClass(code) {
    if (code >= 200 && code < 300) return "s2";
    if (code < 400) return "s3";
    if (code < 500) return "s4";
    return "s5";
  }
  function markChar(st) {
    return st === "passed" ? "✓" : st === "failed" ? "✕" : st === "error" ? "!" :
      st === "running" ? "" : "–"; // skipped 也走这个短横——它既不是通过也不是失败
  }
  var FORMATS = [
    { id: "json", label: "JSON" },
    { id: "yaml", label: "YAML" },
    { id: "raw", label: "原文" }
  ];

  // ── 头部 / 概览 ──
  function renderHead() {
    var r = state.report, s = r.summary;
    var running = r.status === "running";
    var bad = s.failed + s.error;
    var cls = running ? "run" : r.status === "cancelled" ? "warn" : bad ? "bad" : "ok";
    var label = running ? "运行中" : r.status === "cancelled" ? "已取消" :
      bad ? "未全部通过" : "全部通过";
    document.getElementById("badge").className = "badge " + cls;
    document.getElementById("badge").innerHTML = '<span class="dot"></span>' + esc(label);

    var parts = [];
    parts.push("环境 <b>" + esc(r.environment.name || "无") + "</b>");
    // 目标可能有很多（界面里多选、CLI 里给一串）：三个以内全列，超了就收成
    // 「前两项 等 N 项」并给一个可展开的完整列表——头部高度得是恒定的，
    // 但"到底跑了哪些"又必须查得到，那是报告最基本的自证
    var tg = (r.options && r.options.targets) || [];
    if (tg.length) {
      if (tg.length <= 3) {
        parts.push("目标 <b>" + esc(tg.join("、")) + "</b>");
      } else {
        parts.push(
          "目标 <b>" + esc(tg.slice(0, 2).join("、")) + "</b>" +
          ' <button type="button" class="tgt-toggle" id="tgt-toggle" aria-expanded="false">等 ' +
          tg.length + " 项<span class=\"tgt-caret\">▾</span></button>"
        );
      }
    }
    parts.push("开始 <b>" + esc(fmtTime(r.startedAt)) + "</b>");
    parts.push("耗时 <b>" + esc(fmtMs(r.durationMs)) + "</b>");
    if (r.workspace && r.workspace.name) parts.push("工作空间 <b>" + esc(r.workspace.name) + "</b>");
    // 失败传播策略：运行面板可临时覆盖工作空间配置，报告必须自证用的是哪一套——
    // 否则两份结论不同的报告摆在一起，分不清是服务变了还是选项变了
    if (r.options && r.options.continueOnAssertionFailure) {
      parts.push("失败传播 <b>断言失败继续</b>");
    }
    document.getElementById("sub").innerHTML = parts.join("<span>·</span>");

    // 完整目标列表：只在收起过的时候才有内容，点标题切换显隐
    var box = document.getElementById("targets");
    if (tg.length > 3) {
      box.innerHTML = tg.map(function (t) { return '<span class="tgt">' + esc(t) + "</span>"; }).join("");
      var btn = document.getElementById("tgt-toggle");
      btn.onclick = function () {
        var open = box.hidden;
        box.hidden = !open;
        btn.setAttribute("aria-expanded", open ? "true" : "false");
        btn.className = open ? "tgt-toggle is-open" : "tgt-toggle";
      };
    } else {
      box.hidden = true;
      box.innerHTML = "";
    }

    var done = s.passed + s.failed + s.error + s.skipped;
    var bar = document.getElementById("progress");
    if (running) {
      bar.hidden = false;
      var total = Math.max(s.total, done, 1);
      document.getElementById("progress-fill").style.width = Math.round((done / total) * 100) + "%";
    } else {
      bar.hidden = true;
    }

    // 两行一个 grid：上排用例、下排请求，同名状态**列列对齐**，
    // 断言跨两行占满右侧一列（它是唯一不分层级的指标）。顺序即填充顺序，别打乱。
    // 用例级的「跳过」是「这个文件没跑」，请求级的是「上游挂了没轮到它」——
    // 两个维度分开计，但**样式一致**，同一列上下读得是同一件事。
    var t = s.steps || { total: 0, passed: 0, failed: 0, error: 0, skipped: 0 };
    var cards = [
      { n: s.total, l: "用例总数", c: "" },
      { n: s.passed, l: "通过", c: s.passed ? "ok" : "mute" },
      { n: s.failed, l: "失败", c: s.failed ? "bad" : "mute" },
      { n: s.error, l: "错误", c: s.error ? "warn" : "mute" },
      { n: s.skipped, l: "跳过", c: s.skipped ? "warn" : "mute" },
      { n: s.assertions.passed + "/" + s.assertions.total, l: "断言通过",
        c: s.assertions.failed ? "bad" : s.assertions.total ? "ok" : "mute", span: true },
      { n: t.total, l: "请求总数", c: "" },
      { n: t.passed, l: "通过", c: t.passed ? "ok" : "mute" },
      { n: t.failed, l: "失败", c: t.failed ? "bad" : "mute" },
      { n: t.error, l: "错误", c: t.error ? "warn" : "mute" },
      { n: t.skipped, l: "跳过", c: t.skipped ? "warn" : "mute" }
    ];
    document.getElementById("stats").innerHTML = cards.map(function (c) {
      return '<div class="stat' + (c.span ? " stat-tall" : "") + '">' +
        '<div class="stat-num ' + c.c + '">' + esc(c.n) +
        '</div><div class="stat-label">' + esc(c.l) + "</div></div>";
    }).join("");
  }

  // ── 单个 step ──
  function renderStep(st) {
    var h = [];
    h.push('<div class="step">');
    h.push('<div class="step-head">');
    h.push('<span class="mark ' + esc(st.status) + '">' + markChar(st.status) + "</span>");
    h.push('<span class="step-id">' + esc(st.id) + "</span>");
    if (st.request) {
      h.push('<span class="method ' + esc(st.request.method) + '">' + esc(st.request.method) + "</span>");
      h.push('<span class="url" title="' + esc(st.request.url) + '">' + esc(st.request.url) + "</span>");
    }
    h.push('<span class="step-meta">');
    if (st.response) {
      h.push('<span class="status-code ' + statusClass(st.response.status) + '">' +
        esc(st.response.status) + "</span>");
      h.push("<span>" + esc(fmtBytes(st.response.body.bytes)) + "</span>");
    }
    // 跳过的步骤没跑过，"0ms" 是个会误导人的数字
    if (st.status !== "skipped") h.push("<span>" + esc(fmtMs(st.durationMs)) + "</span>");
    h.push("</span></div>");

    if (st.error) h.push('<div class="err">' + esc(st.error) + "</div>");
    // 跳过必须说明原因——报告里一个灰格子不写为什么，看的人无从判断它是没跑还是跑过没事
    if (st.skipReason) h.push('<div class="skip">' + esc(st.skipReason) + "，已跳过</div>");

    if (st.assertions && st.assertions.length) {
      h.push('<table class="asserts"><thead><tr><th class="c-mark"></th><th class="c-target">目标</th>' +
        '<th class="c-op">断言</th><th class="c-expected">期望值</th><th>实际</th></tr></thead><tbody>');
      st.assertions.forEach(function (a) {
        h.push("<tr>");
        h.push('<td class="c-mark"><span class="tick ' + (a.ok ? "y" : "n") + '">' +
          (a.ok ? "✓" : "✕") + "</span></td>");
        h.push('<td class="c-target mono">' + esc(a.target) + "</td>");
        h.push('<td class="c-op">' + esc(opLabel(a.op)) + "</td>");
        h.push('<td class="c-expected mono' + (a.expected === "—" ? " na" : "") + '">' + esc(a.expected) + "</td>");
        h.push('<td class="mono">' + esc(a.actual) + "</td>");
        h.push("</tr>");
      });
      h.push("</tbody></table>");
    }

    if (st.outputs && Object.keys(st.outputs).length) {
      h.push('<details class="detail"><summary>输出 (' + Object.keys(st.outputs).length +
        ")</summary><div class=\"detail-body\"><dl class=\"kv\">");
      Object.keys(st.outputs).forEach(function (k) {
        var v = st.outputs[k];
        h.push("<dt>" + esc(k) + "</dt><dd>" +
          esc(typeof v === "object" ? JSON.stringify(v) : String(v)) + "</dd>");
      });
      h.push("</dl></div></details>");
    }

    if (st.request) h.push(renderPart("请求", st.request.headers, st.request.body, null));
    if (st.response) {
      h.push(renderPart("响应", st.response.headers, st.response.body,
        st.response.status + " " + (st.response.statusText || "")));
    }
    h.push("</div>");
    return h.join("");
  }

  function opLabel(op) {
    var m = { eq: "等于", ne: "不等于", contains: "包含", exists: "存在",
      notExists: "不存在", gt: "大于", lt: "小于", matches: "匹配正则" };
    return m[op] || op;
  }

  function renderPart(title, headers, body, extra) {
    var h = [];
    var hasHeaders = !!(headers && headers.length);
    var hasBody = !!(body && body.preview);
    h.push('<details class="detail"><summary>' + esc(title));
    if (extra) h.push(" · " + esc(extra.trim()));
    if (hasHeaders) h.push(" · " + headers.length + " 个头");
    // 标题上就说明是空的，省得点开才发现里面什么都没有
    if (!hasHeaders && !hasBody) h.push(" · 无头与报文体");
    h.push('</summary><div class="detail-body">');
    if (hasHeaders) {
      h.push('<dl class="kv">');
      headers.forEach(function (kv) {
        h.push("<dt>" + esc(kv.key) + "</dt><dd>" + esc(kv.value) + "</dd>");
      });
      h.push("</dl>");
    }
    if (hasBody) {
      // 能解析成 JSON 才给格式切换；纯文本 / HTML 只给复制，免得点了「YAML」却什么也不变
      var structured = !!ApicaseFmt.asJson(body.preview);
      h.push('<div class="body-bar">');
      if (structured) {
        h.push('<span class="seg mini fmt">');
        FORMATS.forEach(function (f) {
          h.push('<button class="fmt-btn' + (f.id === "json" ? " on" : "") + '" data-fmt="' +
            f.id + '">' + esc(f.label) + "</button>");
        });
        h.push("</span>");
      }
      h.push('<button class="copy-btn" data-copy="1">复制</button></div>');
      // data-raw 存原文：切换格式时从它重新算，不受上一次显示的是哪一种影响
      h.push('<pre class="body" data-raw="' + esc(body.preview) + '">' +
        esc(ApicaseFmt.format(body.preview, "json")) + "</pre>");
      if (body.truncated) {
        h.push('<div class="trunc">已截断，原始大小 ' + esc(fmtBytes(body.bytes)) + "</div>");
      }
    }
    if (!hasHeaders && !hasBody) h.push('<div class="na-note">没有请求头，也没有报文体。</div>');
    h.push("</div></details>");
    return h.join("");
  }

  // ── 单个 case 行 ──
  function caseInner(c) {
    var h = [];
    var asserts = 0, passed = 0;
    (c.steps || []).forEach(function (st) {
      (st.assertions || []).forEach(function (a) { asserts++; if (a.ok) passed++; });
    });
    h.push('<summary>');
    h.push('<svg class="chev" viewBox="0 0 16 16" fill="none" stroke="currentColor" ' +
      'stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M6 3l5 5-5 5"/></svg>');
    h.push('<span class="mark ' + esc(c.status) + '">' + markChar(c.status) + "</span>");
    h.push('<span class="case-name">' + esc(c.name) + "</span>");
    h.push('<span class="case-file" title="' + esc(c.file) + '">' + esc(c.file) + "</span>");
    if (state.host) h.push('<button class="host-btn" data-open="' + esc(c.file) + '">在 apicase 中打开</button>');
    h.push('<span class="case-meta">');
    if (asserts) {
      h.push('<span class="ratio ' + (passed === asserts ? "ok" : "bad") + '">' +
        passed + "/" + asserts + " 断言</span>");
    }
    // 折叠状态下也要看得出「有几步压根没跑」——否则 1/2 断言会被误读成只有两条断言的用例
    if (c.steps && c.steps.length) {
      var skipped = 0;
      c.steps.forEach(function (st) { if (st.status === "skipped") skipped++; });
      h.push("<span>" + c.steps.length + " 步" + (skipped ? " · " + skipped + " 跳过" : "") + "</span>");
    }
    h.push("<span>" + esc(fmtMs(c.durationMs)) + "</span>");
    h.push("</span></summary>");
    h.push('<div class="case-body">');
    if (c.status === "skipped") {
      h.push('<div class="skip-note">已跳过：' + esc(c.skipReason || "未说明原因") + "</div>");
    } else if (!c.steps || !c.steps.length) {
      h.push('<div class="skip-note">没有执行任何请求。</div>');
    } else {
      c.steps.forEach(function (st) { h.push(renderStep(st)); });
    }
    h.push("</div>");
    return h.join("");
  }

  function matches(c) {
    if (state.filter === "failed" && c.status !== "failed" && c.status !== "error") return false;
    if (state.filter === "passed" && c.status !== "passed") return false;
    if (state.filter === "skipped" && c.status !== "skipped") return false;
    if (state.query) {
      var q = state.query.toLowerCase();
      var hay = (c.file + " " + c.name).toLowerCase();
      if (hay.indexOf(q) < 0) {
        // 也搜 step 的 URL——常见诉求是「哪个用例打了这个接口」
        var hit = (c.steps || []).some(function (st) {
          return st.request && st.request.url.toLowerCase().indexOf(q) >= 0;
        });
        if (!hit) return false;
      }
    }
    return true;
  }

  /**
   * 增量同步用例列表：按 file 作 key 复用 <details> 元素。
   * 不整列表重建——否则每次进度更新都会重置用户展开的详情与滚动位置。
   */
  function syncCases() {
    var host = document.getElementById("cases");
    var cases = state.report.cases || [];
    var shown = 0;
    cases.forEach(function (c) {
      var sig = JSON.stringify(c) + "|" + state.host;
      var rec = rows.get(c.file);
      if (!rec) {
        var el = document.createElement("details");
        el.className = "case";
        el.innerHTML = caseInner(c);
        // 失败 / 错误默认展开：打开报告最想看的就是「哪里挂了」，
        // 逐个点开是白费的一步。通过的用例保持收起，列表才扫得动。
        el.open = c.status === "failed" || c.status === "error";
        host.appendChild(el);
        rec = { el: el, sig: sig };
        rows.set(c.file, rec);
      } else if (rec.sig !== sig) {
        var wasOpen = rec.el.open;
        rec.el.innerHTML = caseInner(c);
        // 跑完才判定为失败时也展开：此前它是 running 状态，用户没机会展开它
        rec.el.open = wasOpen || c.status === "failed" || c.status === "error";
        rec.sig = sig;
      }
      var vis = matches(c);
      rec.el.hidden = !vis;
      if (vis) shown++;
    });
    document.getElementById("empty").hidden = shown > 0;
    var hint = document.getElementById("count-hint");
    hint.textContent = shown === cases.length ? "" : "显示 " + shown + " / " + cases.length;
  }

  /**
   * 整份报告的源码视图（JSON / YAML）。
   *
   * 只在切到该视图时才生成：一份跑了几百个用例的报告序列化出来是几 MB 文本，
   * 每次进度更新都算一遍会把页面拖死。
   */
  function renderRaw() {
    var pre = document.getElementById("raw-view");
    if (state.view === "visual" || !state.report) return;
    pre.textContent = state.view === "yaml"
      ? ApicaseFmt.toYaml(state.report)
      : JSON.stringify(state.report, null, 2);
  }

  function applyView() {
    var raw = state.view !== "visual";
    document.getElementById("cases").hidden = raw;
    document.getElementById("toolbar-visual").hidden = raw;
    document.getElementById("raw-view").hidden = !raw;
    document.getElementById("copy-all").hidden = !raw;
    // 「暂无结果」是可视化列表的空态，源码视图下不该冒出来
    if (raw) document.getElementById("empty").hidden = true;
    renderRaw();
    if (!raw) syncCases();
  }

  function render() {
    if (!state.report) return;
    renderHead();
    if (state.view === "visual") syncCases();
    else renderRaw(); // 运行中切在源码视图上：数据每更新一次就重出一份文本
  }

  function setReport(r) {
    if (!r) return;
    state.report = r;
    document.title = "apicase 运行报告 · " + (r.workspace && r.workspace.name ? r.workspace.name : "");
    render();
  }

  // 运行中逐条追加，而不是每完成一个 case 就重收一份整报告：
  // 后者的传输量按报告大小走，跑 N 个用例就是 O(N²) 的结构化克隆。
  // 收尾时宿主仍会推一次整份（带 status / finishedAt），所以这里不必管终态。
  function appendCase(d) {
    if (!state.report || !d.case) return;
    state.report.cases.push(d.case);
    if (d.summary) state.report.summary = d.summary;
    if (typeof d.durationMs === "number") state.report.durationMs = d.durationMs;
    render();
  }

  // ── 事件 ──
  /** 找到与这个按钮同属一块报文体的 <pre> */
  function bodyOf(btn) {
    var bar = btn.closest(".body-bar");
    return bar ? bar.nextElementSibling : null;
  }

  /** 复制到剪贴板：`file://` 下 navigator.clipboard 常被拒，故留一条 textarea 的老路 */
  function copyText(t) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(t).catch(function () { return legacyCopy(t); });
    }
    return Promise.resolve(legacyCopy(t));
  }
  function legacyCopy(t) {
    var ta = document.createElement("textarea");
    ta.value = t;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand("copy"); } catch (err) { /* 两条路都不通就只能让用户手选 */ }
    document.body.removeChild(ta);
  }

  document.addEventListener("click", function (e) {
    var viewBtn = e.target.closest && e.target.closest("[data-view]");
    if (viewBtn) {
      state.view = viewBtn.getAttribute("data-view");
      Array.prototype.forEach.call(document.querySelectorAll("[data-view]"), function (b) {
        b.classList.toggle("on", b === viewBtn);
      });
      applyView();
      return;
    }
    var copyAll = e.target.closest && e.target.closest("[data-copy-all]");
    if (copyAll) {
      copyText(document.getElementById("raw-view").textContent);
      copyAll.textContent = "已复制";
      copyAll.classList.add("done");
      setTimeout(function () {
        copyAll.textContent = "复制全部";
        copyAll.classList.remove("done");
      }, 1400);
      return;
    }
    var fmtBtn = e.target.closest && e.target.closest(".fmt-btn");
    if (fmtBtn) {
      var pre = bodyOf(fmtBtn);
      if (pre) {
        pre.textContent = ApicaseFmt.format(pre.getAttribute("data-raw") || "", fmtBtn.getAttribute("data-fmt"));
        Array.prototype.forEach.call(fmtBtn.parentNode.querySelectorAll("button"), function (b) {
          b.classList.toggle("on", b === fmtBtn);
        });
      }
      return;
    }
    var copyBtn = e.target.closest && e.target.closest(".copy-btn");
    if (copyBtn) {
      var target = bodyOf(copyBtn);
      if (target) {
        // 复制**当前显示的那一份**：切到 YAML 就是为了把 YAML 贴出去
        copyText(target.textContent);
        copyBtn.textContent = "已复制";
        copyBtn.classList.add("done");
        setTimeout(function () {
          copyBtn.textContent = "复制";
          copyBtn.classList.remove("done");
        }, 1400);
      }
      return;
    }
    // 按 data-filter 而不是 `.seg button` 选：报文体的格式切换也是一组 .seg 按钮，
    // 用后者会让点一次筛选就把所有格式按钮的选中态清掉
    var seg = e.target.closest ? e.target.closest("[data-filter]") : null;
    if (seg) {
      state.filter = seg.getAttribute("data-filter");
      Array.prototype.forEach.call(document.querySelectorAll("[data-filter]"), function (b) {
        b.classList.toggle("on", b === seg);
      });
      syncCases();
      return;
    }
    var open = e.target.closest ? e.target.closest("[data-open]") : null;
    if (open && state.host) {
      e.preventDefault();
      e.stopPropagation();
      post({ type: "open-case", file: open.getAttribute("data-open") });
    }
  });
  document.addEventListener("input", function (e) {
    if (e.target && e.target.id === "search") {
      state.query = e.target.value.trim();
      syncCases();
    }
  });

  function post(msg) {
    try { if (window.parent && window.parent !== window) window.parent.postMessage(msg, "*"); }
    catch (err) { /* 无宿主 */ }
  }

  // ── 宿主消息（apicase 内嵌时）──
  // 收不到 host 握手即为浏览器独立打开：IDE 专属动作不渲染。
  window.addEventListener("message", function (e) {
    var d = e.data;
    if (!d || typeof d !== "object") return;
    if (d.type === "host") { state.host = true; render(); return; }
    if (d.type === "theme") {
      if (d.mode === "dark" || d.mode === "light") document.documentElement.setAttribute("data-theme", d.mode);
      else document.documentElement.removeAttribute("data-theme");
      return;
    }
    if (d.type === "report") { setReport(d.report); return; }
    if (d.type === "case") { appendCase(d); return; }
  });

  // 内联数据（落盘的报告自带；空壳则等宿主推送）
  var node = document.getElementById("apicase-report");
  if (node && node.textContent && node.textContent.trim()) {
    try { setReport(JSON.parse(node.textContent)); } catch (err) {
      document.getElementById("empty").textContent = "报告数据损坏，无法解析。";
    }
  }
  post({ type: "ready" });
})();
