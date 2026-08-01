
(function () {
  "use strict";
  var state = { report: null, filter: "all", query: "", host: false };
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
      st === "running" ? "" : "–";
  }
  function pretty(text) {
    if (!text) return text;
    try { return JSON.stringify(JSON.parse(text), null, 2); } catch (e) { return text; }
  }

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
    if (r.options && r.options.targets && r.options.targets.length) {
      parts.push("目标 <b>" + esc(r.options.targets.join("、")) + "</b>");
    }
    parts.push("开始 <b>" + esc(fmtTime(r.startedAt)) + "</b>");
    parts.push("耗时 <b>" + esc(fmtMs(r.durationMs)) + "</b>");
    if (r.workspace && r.workspace.name) parts.push("工作空间 <b>" + esc(r.workspace.name) + "</b>");
    document.getElementById("sub").innerHTML = parts.join("<span>·</span>");

    var done = s.passed + s.failed + s.error + s.skipped;
    var bar = document.getElementById("progress");
    if (running) {
      bar.hidden = false;
      var total = Math.max(s.total, done, 1);
      document.getElementById("progress-fill").style.width = Math.round((done / total) * 100) + "%";
    } else {
      bar.hidden = true;
    }

    var cards = [
      { n: s.total, l: "用例总数", c: "" },
      { n: s.passed, l: "通过", c: s.passed ? "ok" : "mute" },
      { n: s.failed, l: "失败", c: s.failed ? "bad" : "mute" },
      { n: s.error, l: "错误", c: s.error ? "warn" : "mute" },
      { n: s.skipped, l: "跳过", c: s.skipped ? "warn" : "mute" },
      { n: s.assertions.passed + "/" + s.assertions.total, l: "断言通过",
        c: s.assertions.failed ? "bad" : s.assertions.total ? "ok" : "mute" }
    ];
    document.getElementById("stats").innerHTML = cards.map(function (c) {
      return '<div class="stat"><div class="stat-num ' + c.c + '">' + esc(c.n) +
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
    h.push("<span>" + esc(fmtMs(st.durationMs)) + "</span>");
    h.push("</span></div>");

    if (st.error) h.push('<div class="err">' + esc(st.error) + "</div>");

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
      h.push('<pre class="body">' + esc(pretty(body.preview)) + "</pre>");
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
    if (c.steps && c.steps.length) h.push("<span>" + c.steps.length + " 步</span>");
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

  function render() {
    if (!state.report) return;
    renderHead();
    syncCases();
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
  document.addEventListener("click", function (e) {
    var seg = e.target.closest ? e.target.closest(".seg button") : null;
    if (seg) {
      state.filter = seg.getAttribute("data-filter");
      Array.prototype.forEach.call(document.querySelectorAll(".seg button"), function (b) {
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
