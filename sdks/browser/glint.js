/* Glint for the browser, in development. glintd serves this file; add one tag to the page:

     <script src="http://127.0.0.1:23600/v1/browser.js" data-project="shop"></script>

   It reports, with no other code: uncaught errors, unhandled promise rejections, console.error and
   console.warn, failed requests (fetch and XHR: network errors, 5xx as errors, 4xx as warnings) and
   resources that failed to load. window.glint(value) sends values, like the other SDKs, so an agent
   or a developer can place markers in front-end code.

   It never throws into the page, and only talks to glintd, which accepts it from development
   origins (localhost, 127.0.0.1, *.localhost, *.test). */
(() => {
  "use strict";
  if (window.__glint) return;
  const me = document.currentScript;
  const src = new URL((me && me.src) || "http://127.0.0.1:23600/v1/browser.js");
  const base = src.origin;
  const project = (me && me.dataset.project) || src.searchParams.get("project") || undefined;
  const run = "browser-" + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
  const SDK = "glint-browser/0.1.0";
  const realFetch = window.fetch ? window.fetch.bind(window) : null;
  const queue = [];
  let timer = null;
  let sending = false; // our own requests are not reported

  function encode(v, depth = 0, seen = new WeakSet()) {
    if (v === null || typeof v === "number" || typeof v === "boolean") return v;
    if (typeof v === "string") return v.length > 10000 ? v.slice(0, 10000) + "…" : v;
    if (typeof v === "undefined") return null;
    if (typeof v === "bigint") return String(v);
    if (typeof v === "function") return `ƒ ${v.name || "anonymous"}()`;
    if (typeof v === "symbol") return String(v);
    if (v instanceof Error) return { class: v.name, message: v.message, stack: String(v.stack || "") };
    if (typeof Element !== "undefined" && v instanceof Element) {
      return `<${v.tagName.toLowerCase()}${v.id ? "#" + v.id : ""}${v.className && typeof v.className === "string" ? "." + v.className.trim().split(/\s+/).join(".") : ""}>`;
    }
    if (seen.has(v)) return { $ref: "circular" };
    if (depth > 6) return { $truncated: true };
    seen.add(v);
    if (Array.isArray(v)) return v.slice(0, 500).map((x) => encode(x, depth + 1, seen));
    if (v instanceof Map) return Object.fromEntries([...v].slice(0, 500).map(([k, x]) => [String(k), encode(x, depth + 1, seen)]));
    if (v instanceof Set) return [...v].slice(0, 500).map((x) => encode(x, depth + 1, seen));
    const out = {};
    for (const k of Object.keys(v).slice(0, 500)) {
      try {
        out[k] = encode(v[k], depth + 1, seen);
      } catch {
        out[k] = "[unreadable]";
      }
    }
    return out;
  }

  // "at fn (url:line:col)" (Chrome) or "fn@url:line:col" (Firefox, Safari)
  function frames(stack) {
    const out = [];
    for (const line of String(stack || "").split("\n")) {
      const m = line.match(/at (?:(.+?) \()?(.+?):(\d+):(\d+)\)?$/) || line.match(/^(.*?)@(.+?):(\d+):(\d+)$/);
      if (!m) continue;
      const file = m[2];
      out.push({ fn: m[1] || undefined, file, line: +m[3], col: +m[4], vendor: /node_modules|\/@vite\/|\/\.vite\/|webpack-internal|chrome-extension:|\/browser\.js/.test(file) || undefined });
    }
    return out;
  }

  // The caller of glint(): the first frame outside this file.
  function site() {
    const f = frames(new Error().stack).find((x) => !/\/v1\/browser\.js/.test(x.file));
    return f ? { file: f.file, line: f.line, col: f.col, fn: f.fn } : {};
  }

  function send(kind, payload, meta, origin) {
    try {
      const e = { ts: new Date().toISOString(), run, kind, payload, origin: { lang: "js", runtime: "browser", sdk: SDK, host: location.host, ...origin } };
      if (project) e.project = project;
      if (meta && Object.keys(meta).length) e.meta = meta;
      queue.push(e);
      if (queue.length > 500) queue.shift();
      if (!timer) timer = setTimeout(flush, 60);
    } catch {
      // never break the page
    }
  }

  function flush() {
    timer = null;
    if (!queue.length || !realFetch) return;
    const body = JSON.stringify(queue.splice(0));
    sending = true;
    try {
      realFetch(base + "/v1/events", { method: "POST", headers: { "content-type": "application/json" }, body, keepalive: body.length < 60000 }).catch(() => {});
    } catch {
      // glintd is down: drop
    } finally {
      sending = false;
    }
  }
  addEventListener("pagehide", flush);

  function exception(err, label) {
    const e = err instanceof Error ? err : new Error(typeof err === "string" ? err : JSON.stringify(encode(err)));
    const fr = frames(e.stack);
    const top = fr.find((f) => !f.vendor) || fr[0] || {};
    send("core.exception", { class: e.name || "Error", message: e.message, frames: fr }, label ? { label } : {}, { file: top.file, line: top.line, fn: top.fn });
  }

  // ---- what the page does, without code
  addEventListener(
    "error",
    (ev) => {
      if (ev.target && ev.target !== window && (ev.target.src || ev.target.href)) {
        send("core.log", { message: `failed to load ${ev.target.tagName.toLowerCase()} ${ev.target.src || ev.target.href}` }, { level: "warn" }, site());
      } else if (ev.error) {
        exception(ev.error, "uncaught");
      } else if (ev.message) {
        send("core.exception", { class: "Error", message: ev.message, frames: ev.filename ? [{ file: ev.filename, line: ev.lineno, col: ev.colno }] : [] }, { label: "uncaught" }, { file: ev.filename, line: ev.lineno });
      }
    },
    true,
  );
  addEventListener("unhandledrejection", (ev) => exception(ev.reason, "unhandled rejection"));

  for (const level of ["error", "warn"]) {
    const orig = console[level];
    console[level] = function (...args) {
      try {
        const msg = args.map((a) => (typeof a === "string" ? a : a instanceof Error ? `${a.name}: ${a.message}` : JSON.stringify(encode(a)))).join(" ");
        send("core.log", { message: msg.slice(0, 4000) }, { level, label: `console.${level}` }, site());
      } catch {
        // ignore
      }
      return orig.apply(this, args);
    };
  }

  function request(method, url, status, ms, error) {
    const level = error || status >= 500 ? "error" : "warn";
    const what = error ? `failed: ${error}` : `→ ${status}`;
    send("core.log", { message: `${method} ${url} ${what}`, fields: { method, url, status: status || null, duration_ms: Math.round(ms) } }, { level, label: "request" }, site());
  }

  if (realFetch) {
    window.fetch = function (input, init) {
      if (sending) return realFetch(input, init);
      const method = ((init && init.method) || (input && input.method) || "GET").toUpperCase();
      const url = typeof input === "string" ? input : (input && input.url) || String(input);
      const t = performance.now();
      return realFetch(input, init).then(
        (res) => {
          if (res.status >= 400) request(method, url, res.status, performance.now() - t);
          return res;
        },
        (err) => {
          request(method, url, 0, performance.now() - t, (err && err.message) || "network error");
          throw err;
        },
      );
    };
  }
  if (window.XMLHttpRequest) {
    const open = XMLHttpRequest.prototype.open;
    const sendX = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.open = function (method, url, ...rest) {
      this.__glint = { method: String(method).toUpperCase(), url: String(url) };
      return open.call(this, method, url, ...rest);
    };
    XMLHttpRequest.prototype.send = function (...args) {
      const info = this.__glint;
      if (info) {
        const t = performance.now();
        this.addEventListener("loadend", () => {
          if (this.status === 0) request(info.method, info.url, 0, performance.now() - t, "network error");
          else if (this.status >= 400) request(info.method, info.url, this.status, performance.now() - t);
        });
      }
      return sendX.apply(this, args);
    };
  }

  // ---- markers
  function glint(...values) {
    const at = site();
    for (const v of values) {
      if (v instanceof Error) exception(v);
      else send("core.value", { value: encode(v) }, {}, at);
    }
  }
  glint.log = (message, fields, level = "info") => send("core.log", fields === undefined ? { message: String(message) } : { message: String(message), fields: encode(fields) }, { level }, site());
  glint.exception = (err) => exception(err);
  glint.table = (rows) => send("core.table", { rows: encode(Array.from(rows || [])) }, {}, site());
  glint.flush = flush;
  window.__glint = { run, project, flush };
  if (typeof window.glint === "undefined") window.glint = glint;
  send("core.log", { message: `${location.pathname}${location.search}`, fields: { url: location.href, title: document.title } }, { label: "page" }, {});
})();
