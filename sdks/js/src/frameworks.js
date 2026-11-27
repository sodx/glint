// Framework integrations for Node, with no code: the global shim (sdks/global/global.mjs) calls
// install() and every HTTP server in the process reports on its own.
//
// - Each request an http/https server handles (Express, Next.js, Fastify, Koa, plain node:http)
//   is a run with one root event, a core.measure named like "GET /orders/:id → 500", with method,
//   route, status, time, memory and number of queries; what the request sends nests under it.
//   Other servers than Express and Next.js (a dev server serving assets, a proxy) report only the
//   requests that sent something or failed with a 5xx.
// - Express: errors that reach error handling, with the request they happened in (an error
//   middleware is placed before the app's first error handler at its first request).
// - Errors a framework only prints (Next.js logs route errors with console.error) are reported
//   as exceptions when they are printed while a request runs.
// - Failed fetch() calls: network errors, 4xx and 5xx.
// - SQL of pg, mysql2 and node:sqlite, with the app line that ran it.
//
// GLINT_FRAMEWORKS=off turns all of it off. Off when the SDK is (NODE_ENV=production).

import { internal } from "./index.js";

const { config, ulid, emit, current, withContext, parseStack, exceptionPayload, encode, SDK_PATH } = internal;
const reported = new WeakSet();
const SKIP = /^\/(_next\/|__nextjs|__webpack_hmr|@vite\/|@fs\/|@id\/|node_modules\/|favicon\.ico)/;
let installed = false;

const now = () => performance.now();
const isOff = () => !config.enabled || /^(0|off|false|no)$/i.test(process.env.GLINT_FRAMEWORKS || "");

/** The first frame of the app's own code: outside node_modules, Node itself and Glint. */
function appSite(stack = new Error().stack) {
  const f = parseStack(stack).find((x) => x.file && !x.vendor && !x.file.startsWith(SDK_PATH) && !x.file.startsWith("node:") && !x.file.includes("/sdks/global/") && !x.file.endsWith("global.mjs"));
  return f ? { file: f.file, line: f.line, col: f.col, fn: f.fn } : {};
}

function framework() {
  if (process.env.NEXT_RUNTIME || process.env.__NEXT_PRIVATE_ORIGIN || process.argv.some((a) => /[\\/]next[\\/]/.test(a))) return "Next.js";
  return null;
}

// ---------------------------------------------------------------- requests

function wrapRequest(server, req, res, handle) {
  const path = String(req.url || "/").split("?")[0];
  if (isOff() || current().req || SKIP.test(path)) return handle();
  const root = ulid();
  // stats is shared by reference: contexts derived from this one (pools) count into it.
  const ctx = { run: `js-${root}`, parent: root, req, stats: { queries: 0, sqlMs: 0, events: 0, label: framework() }, t0: now() };
  const listener = server.listeners("request")[0];
  if (listener && typeof listener.handle === "function" && typeof listener.use === "function" && typeof listener.set === "function") {
    ctx.stats.label = "Express";
    patchExpress(listener);
  }
  let done = false;
  const finish = () => {
    if (done) return;
    done = true;
    try {
      const status = res.headersSent || res.writableFinished ? res.statusCode : null;
      let route = null;
      if (req.route && typeof req.route.path === "string") route = (req.baseUrl || "") + req.route.path;
      // A plain node:http server (a dev server serving assets, a proxy) reports only requests
      // that sent something or failed; Express and Next.js report every request.
      if (!ctx.stats.label && !ctx.stats.events && !(status >= 500)) return;
      const label = ctx.stats.label || "Node HTTP";
      const name = `${req.method} ${route || path}${status ? ` → ${status}` : ""}${res.writableFinished ? "" : " (closed early)"}`;
      const payload = {
        name,
        duration_ms: Math.round((now() - ctx.t0) * 10) / 10,
        memory_bytes: process.memoryUsage().rss,
        method: req.method,
        url: `${req.socket && req.socket.encrypted ? "https" : "http"}://${req.headers.host || ""}${req.url}`,
        route: route || undefined,
        status: status || undefined,
        queries: ctx.stats.queries,
        sql_ms: Math.round(ctx.stats.sqlMs * 100) / 100,
      };
      const meta = { label };
      if (status >= 500) meta.level = "error";
      withContext({ run: ctx.run }, () => emit("core.measure", payload, meta, root, {}));
    } catch {
      // never break the server
    }
  };
  res.once("finish", finish);
  res.once("close", finish);
  return withContext(ctx, handle);
}

function patchServers(mod) {
  const proto = mod.Server && mod.Server.prototype;
  if (!proto || proto.__glint) return;
  proto.__glint = true;
  const original = proto.emit;
  proto.emit = function (event, req, res) {
    if (event !== "request" || !res) return original.apply(this, arguments);
    const args = arguments;
    return wrapRequest(this, req, res, () => original.apply(this, args));
  };
}

// ---------------------------------------------------------------- exceptions

function reportError(err, extra) {
  if (isOff() || !err || typeof err !== "object" || reported.has(err)) return;
  reported.add(err);
  try {
    const p = exceptionPayload(err);
    const ctx = current();
    const context = { ...extra };
    if (ctx.req) {
      context.method = ctx.req.method;
      context.url = ctx.req.url;
      if (ctx.req.route && ctx.req.route.path) context.route = (ctx.req.baseUrl || "") + ctx.req.route.path;
    }
    if (Object.keys(context).length) p.context = encode(context);
    const f = (p.frames || []).find((x) => x.file && !x.vendor && !x.file.startsWith("node:"));
    emit("core.exception", p, { level: "error", label: (ctx.stats && ctx.stats.label) || "Node" }, undefined, f ? { file: f.file, line: f.line, col: f.col, fn: f.fn } : {});
  } catch {
    // never break the app
  }
}

const expressApps = new WeakSet();
function patchExpress(app) {
  if (expressApps.has(app)) return;
  expressApps.add(app);
  try {
    const router = app._router || app.router;
    if (!router || !Array.isArray(router.stack)) return;
    // eslint-disable-next-line no-unused-vars
    app.use(function glintErrors(err, req, res, next) {
      reportError(err);
      next(err);
    });
    const stack = router.stack;
    const layer = stack.pop();
    const i = stack.findIndex((l) => l && typeof l.handle === "function" && l.handle.length === 4);
    stack.splice(i < 0 ? stack.length : i, 0, layer);
  } catch {
    // an Express we do not know: requests are still reported
  }
}

function patchConsole() {
  const original = console.error;
  console.error = function (...args) {
    try {
      if (current().req) for (const a of args) if (a instanceof Error) reportError(a);
    } catch {
      // ignore
    }
    return original.apply(this, args);
  };
}

// ---------------------------------------------------------------- fetch

function patchFetch() {
  const original = globalThis.fetch;
  if (typeof original !== "function" || original.__glint) return;
  const patched = function fetch(input, init) {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : (input && input.url) || String(input);
    if (isOff() || url.startsWith(config.url.replace(/\/$/, ""))) return original.apply(this, arguments);
    const method = String((init && init.method) || (input && input.method) || "GET").toUpperCase();
    const site = appSite();
    const t0 = now();
    const failed = (status, error) => {
      const p = { method, url, duration_ms: Math.round((now() - t0) * 10) / 10 };
      if (status) p.status = status;
      if (error) p.error = `${error.name || "Error"}: ${error.message}${error.cause && error.cause.message ? ` (${error.cause.message})` : ""}`;
      emit("http.request", p, { level: error || status >= 500 ? "error" : "warn", label: "fetch" }, undefined, site);
    };
    return original.apply(this, arguments).then(
      (res) => {
        if (res.status >= 400) failed(res.status);
        return res;
      },
      (err) => {
        if (!(err && err.name === "AbortError")) failed(null, err);
        throw err;
      },
    );
  };
  patched.__glint = true;
  globalThis.fetch = patched;
}

// ---------------------------------------------------------------- SQL

function sql(text, bindings, conn, ms, rows, error, site) {
  const ctx = current();
  if (ctx.stats) {
    ctx.stats.queries++;
    ctx.stats.sqlMs += ms;
  }
  const p = { sql: String(text), duration_ms: Math.round(ms * 1000) / 1000, conn };
  if (bindings && (!Array.isArray(bindings) || bindings.length)) p.bindings = encode(bindings);
  if (Number.isInteger(rows)) p.rows = rows;
  if (error) p.error = String(error && error.message ? error.message : error);
  emit("sql.query", p, error ? { level: "error" } : undefined, undefined, site);
}

/** Wraps a query method that takes (text | {text, values}, values?, callback?) or returns a promise. */
function wrapQuery(proto, method, conn, textOf, rowsOf) {
  const original = proto[method];
  if (typeof original !== "function" || original.__glint) return;
  const patched = function (...args) {
    if (isOff()) return original.apply(this, args);
    let text, values;
    try {
      [text, values] = textOf(args);
    } catch {
      return original.apply(this, args);
    }
    if (!text) return original.apply(this, args);
    let site = appSite();
    if (!site.file && current().sqlSite) site = current().sqlSite; // called from a pool
    const t0 = now();
    const done = (err, res) => {
      try {
        sql(text, values, conn, now() - t0, err ? undefined : rowsOf(res), err, site);
      } catch {
        // ignore
      }
    };
    const cb = args.length && typeof args[args.length - 1] === "function" ? args.length - 1 : -1;
    if (cb >= 0) {
      const user = args[cb];
      args[cb] = function (err, res) {
        done(err, res);
        return user.apply(this, arguments);
      };
      return original.apply(this, args);
    }
    const out = original.apply(this, args);
    if (out && typeof out.then === "function") {
      out.then((r) => done(null, r), (e) => done(e));
    } else if (out && typeof out.once === "function") {
      out.once("end", () => done(null, null));
      out.once("error", (e) => done(e));
    }
    return out;
  };
  patched.__glint = true;
  proto[method] = patched;
}

/** Pools run the query later, from a callback: remember the app line that asked for it. */
function wrapPool(proto, method) {
  const original = proto && proto[method];
  if (typeof original !== "function" || original.__glint) return;
  const patched = function (...args) {
    if (isOff()) return original.apply(this, args);
    const site = appSite();
    return withContext({ ...current(), sqlSite: site }, () => original.apply(this, args));
  };
  patched.__glint = true;
  proto[method] = patched;
}

function patchPg(pg) {
  const Client = pg && (pg.Client || (pg.default && pg.default.Client));
  if (!Client) return;
  const Pool = pg.Pool || (pg.default && pg.default.Pool);
  if (Pool) wrapPool(Pool.prototype, "query");
  wrapQuery(Client.prototype, "query", "pg", (a) => (typeof a[0] === "string" ? [a[0], Array.isArray(a[1]) ? a[1] : undefined] : [a[0] && a[0].text, a[0] && a[0].values]), (r) => (r && Number.isInteger(r.rowCount) ? r.rowCount : undefined));
}

function patchMysql2(mysql) {
  const textOf = (a) => (typeof a[0] === "string" ? [a[0], Array.isArray(a[1]) ? a[1] : undefined] : [a[0] && a[0].sql, a[0] && a[0].values]);
  const rowsOf = (r) => (Array.isArray(r) ? r.length : r && Number.isInteger(r.affectedRows) ? r.affectedRows : undefined);
  if (mysql && mysql.Pool) {
    wrapPool(mysql.Pool.prototype, "query");
    wrapPool(mysql.Pool.prototype, "execute");
  }
  for (const C of [mysql && mysql.Connection, mysql && mysql.PoolConnection]) {
    if (!C) continue;
    wrapQuery(C.prototype, "query", "mysql2", textOf, rowsOf);
    wrapQuery(C.prototype, "execute", "mysql2", textOf, rowsOf);
  }
}

function patchSqlite(sqlite) {
  const S = sqlite && sqlite.StatementSync;
  const D = sqlite && sqlite.DatabaseSync;
  if (!S || S.prototype.__glint) return;
  S.prototype.__glint = true;
  for (const m of ["run", "get", "all"]) {
    const original = S.prototype[m];
    S.prototype[m] = function (...args) {
      if (isOff()) return original.apply(this, args);
      const site = appSite();
      const t0 = now();
      try {
        const r = original.apply(this, args);
        sql(this.sourceSQL, args, "sqlite", now() - t0, m === "all" ? r.length : m === "run" ? r && Number(r.changes) : r ? 1 : 0, null, site);
        return r;
      } catch (e) {
        sql(this.sourceSQL, args, "sqlite", now() - t0, undefined, e, site);
        throw e;
      }
    };
  }
  if (D) {
    const exec = D.prototype.exec;
    D.prototype.exec = function (text) {
      if (isOff()) return exec.apply(this, arguments);
      const site = appSite();
      const t0 = now();
      try {
        const r = exec.apply(this, arguments);
        sql(text, undefined, "sqlite", now() - t0, undefined, null, site);
        return r;
      } catch (e) {
        sql(text, undefined, "sqlite", now() - t0, undefined, e, site);
        throw e;
      }
    };
  }
}

const PACKAGES = { pg: patchPg, mysql2: patchMysql2 };

async function patchModules() {
  const Module = (await import("node:module")).default;
  const { createRequire } = Module;
  let busy = false;
  const seen = new Set();
  const onResolve = (specifier, url) => {
    if (busy) return;
    if (specifier === "node:sqlite" || specifier === "sqlite") {
      if (!seen.has("sqlite")) {
        seen.add("sqlite");
        patchSqlite(process.getBuiltinModule ? process.getBuiltinModule("node:sqlite") : null);
      }
      return;
    }
    const name = specifier.startsWith("mysql2") ? "mysql2" : specifier === "pg" ? "pg" : null;
    if (!name || seen.has(url) || !url || !url.startsWith("file:")) return;
    seen.add(url);
    busy = true;
    try {
      // Load it now (CommonJS: the import that follows gets this same object) and patch it.
      const path = new URL(url).pathname;
      PACKAGES[name](createRequire(url)(name === "mysql2" ? path.replace(/promise\.js$/, "index.js") : path));
    } catch {
      // a version we do not know: no SQL, nothing else changes
    } finally {
      busy = false;
    }
  };
  if (typeof Module.registerHooks === "function") {
    Module.registerHooks({
      resolve(specifier, context, next) {
        const r = next(specifier, context);
        try {
          onResolve(specifier, r && r.url);
        } catch {
          // ignore
        }
        return r;
      },
    });
  } else {
    // Older Node: require() only.
    const load = Module._load;
    Module._load = function (request, parent) {
      const out = load.apply(this, arguments);
      try {
        if (request === "pg") patchPg(out);
        else if (request === "mysql2" || request === "mysql2/promise") patchMysql2(load.call(this, "mysql2", parent));
        else if (request === "node:sqlite") patchSqlite(out);
      } catch {
        // ignore
      }
      return out;
    };
  }
}

/** Called once by the global shim. Never throws. */
export async function install() {
  if (installed || typeof process === "undefined" || isOff()) return;
  installed = true;
  try {
    const http = await import("node:http");
    const https = await import("node:https");
    patchServers(http.default || http);
    patchServers(https.default || https);
    patchConsole();
    patchFetch();
    await patchModules();
  } catch {
    // a debugging aid never changes what the program does
  }
}

export { reportError, appSite };
