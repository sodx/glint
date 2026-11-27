// Glint SDK for JavaScript and TypeScript (Node >= 18; browsers with fetch).
//
//   import glint from "glint-sdk";
//   glint(order).label("before save").orange();
//   glint.log("user signed in", { userId: 7 }, "info");
//
// Contract: spec/conformance/README.md. Never throws into your program, never blocks it.

import { encode, typeName } from "./encode.js";

export const VERSION = "0.1.0";
const SDK = `glint-js/${VERSION}`;
const DEFAULT_URL = "http://127.0.0.1:23600";
const TIMEOUT_MS = 2000;
const RETRY_AFTER_MS = 5000;
const MAX_QUEUE = 10_000;
const COLORS = ["gray", "green", "orange", "red", "blue", "purple"];

const env = (k) => (typeof process !== "undefined" && process.env ? process.env[k] : undefined);
const SDK_DIR = new URL(".", import.meta.url).href;

const config = {
  url: env("GLINT_URL") || DEFAULT_URL,
  enabled: isEnabled(),
  project: env("GLINT_PROJECT"),
};

function isEnabled() {
  const v = (env("GLINT_ENABLED") || "").toLowerCase();
  if (v === "0" || v === "false" || v === "off") return false;
  if (v === "1" || v === "true" || v === "on") return true;
  return env("NODE_ENV") !== "production";
}

// ---------------------------------------------------------------- ids

const B32 = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
function ulid() {
  let t = Date.now();
  let time = "";
  for (let i = 0; i < 10; i++) {
    time = B32[t % 32] + time;
    t = Math.floor(t / 32);
  }
  const rnd = new Uint8Array(16);
  if (globalThis.crypto && globalThis.crypto.getRandomValues) globalThis.crypto.getRandomValues(rnd);
  else for (let i = 0; i < 16; i++) rnd[i] = Math.floor(Math.random() * 256);
  let r = "";
  for (let i = 0; i < 16; i++) r += B32[rnd[i] % 32];
  return time + r;
}

const RUN = `js-${ulid()}`;

// ---------------------------------------------------------------- call site

/** Parses V8 / SpiderMonkey / JSC stack lines into frames. */
export function parseStack(stack) {
  const frames = [];
  for (const raw of String(stack || "").split("\n")) {
    const line = raw.trim();
    let m = line.match(/^at (?:(.*?) \()?(.*?):(\d+):(\d+)\)?$/); // V8
    if (!m) {
      const f = line.match(/^(.*?)@(.*?):(\d+):(\d+)$/); // Firefox / Safari
      if (f) m = [f[0], f[1], f[2], f[3], f[4]];
    }
    if (!m) continue;
    const file = toPath(m[2]);
    frames.push({
      file,
      line: Number(m[3]),
      col: Number(m[4]),
      fn: m[1] || undefined,
      vendor: /node_modules|^node:|^internal\//.test(file) || undefined,
    });
  }
  return frames;
}

function toPath(f) {
  if (f.startsWith("file://")) {
    try {
      return decodeURIComponent(new URL(f).pathname);
    } catch {
      return f;
    }
  }
  return f.replace(/^async /, "");
}

const SDK_PATH = toPath(SDK_DIR);

function callSite() {
  const frames = parseStack(new Error().stack);
  return frames.find((f) => !f.file.startsWith(SDK_PATH)) || {};
}

function origin() {
  const site = callSite();
  const o = { lang: "js", sdk: SDK };
  if (typeof process !== "undefined" && process.versions && process.versions.node) {
    o.runtime = `node ${process.versions.node}`;
    o.pid = process.pid;
    try {
      o.cwd = process.cwd();
    } catch {
      // cwd can vanish; not worth failing over.
    }
  } else if (typeof navigator !== "undefined") {
    o.runtime = navigator.userAgent;
  }
  if (site.file) o.file = site.file;
  if (site.line) o.line = site.line;
  if (site.col) o.col = site.col;
  if (site.fn) o.fn = site.fn;
  return o;
}

// ---------------------------------------------------------------- transport

const queue = [];
const inFlight = new Set();
let scheduled = false;
let downUntil = 0;
let dropped = 0;
let warned = false;

function goDown() {
  downUntil = Date.now() + RETRY_AFTER_MS;
  if (warned) return;
  warned = true;
  // Node only: one line on stderr. Browsers stay quiet; the console is the user's.
  try {
    if (typeof process !== "undefined" && process.stderr && process.stderr.write) {
      process.stderr.write(`glint: cannot reach ${config.url}; events are dropped until it answers\n`);
    }
  } catch {
    // ignore
  }
}

function enqueue(envelope) {
  if (queue.length >= MAX_QUEUE) {
    queue.shift();
    dropped++;
  }
  queue.push(envelope);
  if (!scheduled) {
    scheduled = true;
    // Everything a synchronous block sends goes out in one request.
    queueMicrotask(drain);
  }
}

function drain() {
  scheduled = false;
  if (!queue.length) return;
  const batch = queue.splice(0, queue.length);
  if (Date.now() < downUntil) {
    dropped += batch.length;
    return;
  }
  if (dropped) {
    batch[0].meta = { ...batch[0].meta, dropped };
    dropped = 0;
  }
  let p;
  try {
    p = fetch(`${config.url.replace(/\/$/, "")}/v1/events`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(batch),
      keepalive: typeof window !== "undefined",
      signal: typeof AbortSignal !== "undefined" && AbortSignal.timeout ? AbortSignal.timeout(TIMEOUT_MS) : undefined,
    })
      .then((res) => {
        if (!res.ok && res.status >= 500) goDown();
        return res.body && res.body.cancel ? res.body.cancel() : undefined;
      })
      .catch(goDown);
  } catch {
    goDown();
    return;
  }
  inFlight.add(p);
  p.finally(() => inFlight.delete(p));
}

// ---------------------------------------------------------------- envelopes

/* Nesting: everything sent inside glint.measure(name, fn) has that measure as its parent, so a
   run reads as a call tree. In Node the measure follows fn across awaits (AsyncLocalStorage);
   elsewhere only fn's synchronous part is nested. */
let scope = null;
try {
  const { AsyncLocalStorage } = await import("node:async_hooks");
  scope = new AsyncLocalStorage();
} catch {
  // not Node: a plain stack below
}
// Node's require, for flushSync's child process; absent elsewhere.
let nodeRequire = null;
try {
  const { createRequire } = await import("node:module");
  nodeRequire = createRequire(import.meta.url);
} catch {
  // not Node
}

/* The store is { run?, parent? }: framework integrations (src/frameworks.js) give each request a
   run of its own and a root event to nest under. */
const stack = [];
const current = () => (scope ? scope.getStore() : stack[stack.length - 1]) || {};
const currentParent = () => current().parent;
function within(id, fn) {
  return withContext({ ...current(), parent: id }, fn);
}
function withContext(ctx, fn) {
  if (scope) return scope.run(ctx, fn);
  stack.push(ctx);
  try {
    return fn();
  } finally {
    stack.pop();
  }
}

function emit(kind, payload, meta, fixedId, site) {
  if (!config.enabled) return null;
  try {
    const id = fixedId || ulid();
    const o = origin();
    if (site) {
      for (const k of ["file", "line", "col", "fn"]) delete o[k];
      Object.assign(o, site);
    }
    const e = { id, ts: new Date().toISOString(), run: current().run || RUN, kind, payload, origin: o };
    const parent = currentParent();
    if (parent && parent !== id) e.parent = parent;
    if (meta && Object.keys(meta).length) e.meta = meta;
    if (config.project) e.project = config.project;
    enqueue(e);
    const stats = current().stats;
    if (stats) stats.events++; // a framework integration's request (src/frameworks.js)
    return id;
  } catch {
    return null; // never let a debugging aid break the program
  }
}

/** Handle for events just sent; its methods change how they are shown. */
export class Entry {
  constructor(ids) {
    this.ids = ids.filter(Boolean);
  }
  #update(change) {
    for (const target of this.ids) emit("core.update", { target, ...change });
    return this;
  }
  label(label) {
    return this.#update({ meta: { label: String(label) } });
  }
  color(color) {
    return COLORS.includes(color) ? this.#update({ meta: { color } }) : this;
  }
  gray() { return this.color("gray"); }
  green() { return this.color("green"); }
  orange() { return this.color("orange"); }
  red() { return this.color("red"); }
  blue() { return this.color("blue"); }
  purple() { return this.color("purple"); }
  small() { return this.#update({ meta: { size: "sm" } }); }
  large() { return this.#update({ meta: { size: "lg" } }); }
  hide() { return this.#update({ hidden: true }); }
  remove() { return this.#update({ removed: true }); }
}

function valuePayload(v) {
  const p = { value: encode(v) };
  const t = typeName(v);
  if (t) p.type = t;
  return p;
}

function exceptionPayload(err, depth = 0) {
  if (!(err instanceof Error)) {
    return { class: typeof err, message: String(err), frames: [] };
  }
  const p = {
    class: err.name || (err.constructor && err.constructor.name) || "Error",
    message: err.message,
    frames: parseStack(err.stack),
  };
  if (err.cause instanceof Error && depth < 5) p.cause = exceptionPayload(err.cause, depth + 1);
  return p;
}

/** Sends each value as its own event; an Error becomes an exception (as glint.exception). */
export function glint(...values) {
  return new Entry(values.map((v) => (v instanceof Error ? emit("core.exception", exceptionPayload(v)) : emit("core.value", valuePayload(v)))));
}

glint.log = (message, fields, level = "info") => {
  const p = { message: String(message) };
  if (fields !== undefined && fields !== null) p.fields = encode(fields);
  return new Entry([emit("core.log", p, { level })]);
};

glint.table = (rows, columns) => {
  const p = { rows: encode(Array.from(rows || [])) };
  if (columns) p.columns = Array.from(columns, String);
  return new Entry([emit("core.table", p)]);
};

glint.exception = (err) => new Entry([emit("core.exception", exceptionPayload(err))]);

glint.measure = (name, fn) => {
  const now = () => (typeof performance !== "undefined" ? performance.now() : Date.now());
  const start = now();
  // The id is chosen up front so that what fn sends can name the measure as its parent.
  const id = config.enabled ? ulid() : null;
  const done = () => emit("core.measure", { name: String(name), duration_ms: Math.round((now() - start) * 1000) / 1000 }, undefined, id);
  const result = id ? within(id, fn) : fn();
  if (result && typeof result.then === "function") {
    return result.finally(done);
  }
  done();
  return result;
};

glint.send = (kind, payload, meta) => new Entry([emit(String(kind), encode(payload), meta ? encode(meta) : undefined)]);

/** Thrown by `glint.pause()` when someone answers the pause with "stop". */
export class GlintStop extends Error {
  constructor() {
    super("glint: stop requested");
    this.name = "GlintStop";
  }
}

/**
 * Waits until someone continues the program from the Glint UI or an agent (MCP
 * `glint_resume`). Resolves with the value they passed, if any. Skipped (resolves at once)
 * when disabled, when Glint is unreachable, and on CI unless GLINT_FORCE_PAUSE=1.
 */
glint.pause = async (reason, { timeoutMs = 300_000 } = {}) => {
  if (!config.enabled) return undefined;
  if (env("CI") && !env("GLINT_FORCE_PAUSE")) return undefined;
  const id = emit("ctl.pause", reason === undefined ? {} : { reason: String(reason) });
  if (!id) return undefined;
  await glint.flush(); // the pause must be stored before we ask about it
  const base = config.url.replace(/\/$/, "");
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    let res;
    try {
      res = await fetch(`${base}/v1/ctl/${encodeURIComponent(id)}?timeout_s=25`, {
        signal: AbortSignal.timeout ? AbortSignal.timeout(30_000) : undefined,
      });
    } catch {
      return undefined; // Glint went away: carry on
    }
    if (res.status === 204) continue;
    if (res.status !== 200) return undefined;
    const answer = await res.json();
    if (answer.action === "stop") throw new GlintStop();
    return answer.value;
  }
  return undefined;
};

/** Resolves once everything queued so far has been sent, or after `timeoutMs`. */
/**
 * Last words of a crashing process: sends what is queued now and waits for it, synchronously (a
 * short-lived child process makes the request), because a crashing process exits before any
 * asynchronous request could finish. Node only; elsewhere it does nothing.
 */
glint.flushSync = (timeoutMs = 1500) => {
  if (!queue.length || !nodeRequire || typeof process === "undefined") return;
  const batch = queue.splice(0, queue.length);
  try {
    const { spawnSync } = nodeRequire("node:child_process");
    const script = `let b="";process.stdin.on("data",d=>b+=d).on("end",()=>fetch(process.argv[1],{method:"POST",headers:{"content-type":"application/json"},body:b}).catch(()=>{}))`;
    spawnSync(process.execPath, ["-e", script, `${config.url.replace(/\/$/, "")}/v1/events`], {
      input: JSON.stringify(batch),
      timeout: timeoutMs,
      stdio: ["pipe", "ignore", "ignore"],
      env: { ...process.env, NODE_OPTIONS: "" },
    });
  } catch {
    // never fail the crash itself
  }
};

glint.flush = (timeoutMs = TIMEOUT_MS) => {
  drain();
  const all = Promise.allSettled(Array.from(inFlight));
  return Promise.race([all, new Promise((r) => setTimeout(r, timeoutMs).unref?.())]).then(() => undefined);
};

glint.configure = (opts = {}) => {
  if (opts.url) config.url = opts.url;
  if (typeof opts.enabled === "boolean") config.enabled = opts.enabled;
  if (opts.project) config.project = opts.project;
};

/** For framework integrations (src/frameworks.js); not a public API. */
export const internal = {
  config,
  ulid,
  emit,
  current,
  withContext,
  parseStack,
  exceptionPayload,
  encode,
  SDK_PATH,
};

export default glint;
