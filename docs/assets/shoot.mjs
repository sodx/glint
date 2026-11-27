// Makes the README pictures: the banner and framed screenshots of a running glintd, with headless
// Chrome over the DevTools protocol (Node 22+ has WebSocket built in, so nothing to install).
//
//   node docs/assets/shoot.mjs http://127.0.0.1:23730 [http://127.0.0.1:23731]
//
// The first glintd should hold some runs (python3 examples/e2e/run.py with GLINT_URL pointing at
// it, and a few MCP calls for the Agents view); the optional second one is empty, for the
// first-run screen. Writes docs/assets/*.png at 2x.
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const [base, empty] = process.argv.slice(2);
if (!base) { console.error("usage: node docs/assets/shoot.mjs <glintd url> [empty glintd url]"); process.exit(2); }
const out = dirname(fileURLToPath(import.meta.url));
const chromePath = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const work = mkdtempSync(join(tmpdir(), "glint-shoot-"));
const port = 9333;
const chrome = spawn(chromePath, ["--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${join(work, "profile")}`,
  "--hide-scrollbars", "--no-first-run", "--lang=en-US", "--allow-file-access-from-files", "about:blank"], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 50; i++) {
    try {
      const page = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === "page");
      if (page) return page.webSocketDebuggerUrl;
    } catch {}
    await sleep(200);
  }
  throw new Error("Chrome did not start");
}

const ws = new WebSocket(await target());
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let seq = 0;
const waiting = new Map();
ws.addEventListener("message", (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && waiting.has(msg.id)) { waiting.get(msg.id)(msg); waiting.delete(msg.id); }
});
const cdp = (method, params = {}) => new Promise((resolve, reject) => {
  const id = ++seq;
  waiting.set(id, (msg) => (msg.error ? reject(new Error(`${method}: ${msg.error.message}`)) : resolve(msg.result)));
  ws.send(JSON.stringify({ id, method, params }));
});
const js = async (expression) => {
  const r = await cdp("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || expression);
  return r.result.value;
};

async function open(url, { width = 1280, height = 800, dark = true } = {}) {
  await cdp("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 2, mobile: false });
  await cdp("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: dark ? "dark" : "light" }] });
  await cdp("Page.navigate", { url });
  await sleep(2500);
  // The key hints at the bottom are for people at the keyboard, not for a picture.
  await js(`(() => { const s = document.createElement("style"); s.textContent = ".keys { display: none !important; }"; document.head.append(s); return document.fonts.ready.then(() => true); })()`);
}
async function capture(file) {
  await sleep(700);
  const r = await cdp("Page.captureScreenshot", { format: "png" });
  writeFileSync(file, Buffer.from(r.data, "base64"));
}

// Glint's colours (ui/index.html and app/icons/make.py) and its four-point spark.
const SPARK = "M12 1.5l1.9 8.6 8.6 1.9-8.6 1.9L12 22.5l-1.9-8.6L1.5 12l8.6-1.9z";
const FONTS = `<link href="https://fonts.googleapis.com/css2?family=Mona+Sans:wdth,wght@75..125,300..800&family=JetBrains+Mono:wght@400;500&display=swap" rel="stylesheet">`;
const theme = (dark) => dark
  ? { bg: "#0e0f13", glow: "rgba(214,245,84,.16)", glow2: "rgba(120,140,255,.10)", text: "#f3f4ee", muted: "#a2a6ad", line: "rgba(255,255,255,.09)", bar: "#1b1d24" }
  : { bg: "#f4f5ef", glow: "rgba(150,190,20,.22)", glow2: "rgba(90,110,230,.10)", text: "#14161b", muted: "#5d6169", line: "rgba(0,0,0,.09)", bar: "#e9eae4" };

async function page(name, html, width, height, dark) {
  const file = join(work, `${name}.html`);
  writeFileSync(file, `<!doctype html><meta charset="utf-8">${FONTS}<style>html,body{margin:0}</style>${html}`);
  await open(pathToFileURL(file).href, { width, height, dark });
  await capture(join(out, `${name}.png`));
  console.log("wrote", `${name}.png`);
}

// A screenshot inside a window, on a soft background in Glint's colours.
async function framed(name, raw, { title, dark = true, width = 1280 } = {}) {
  const t = theme(dark);
  const html = `<body style="background:${t.bg};background-image:radial-gradient(60% 70% at 15% 0%,${t.glow},transparent 70%),radial-gradient(50% 60% at 100% 100%,${t.glow2},transparent 70%);padding:56px 64px;box-sizing:border-box;width:${width + 128}px">
  <div style="border-radius:14px;overflow:hidden;border:1px solid ${t.line};box-shadow:0 30px 80px -20px rgba(0,0,0,${dark ? ".7" : ".25"}),0 0 0 1px ${t.line}">
    <div style="height:38px;background:${t.bar};display:flex;align-items:center;gap:8px;padding:0 14px;position:relative;font:500 13px 'Mona Sans',system-ui;color:${t.muted}">
      <i style="width:12px;height:12px;border-radius:50%;background:#ff5f57"></i><i style="width:12px;height:12px;border-radius:50%;background:#febc2e"></i><i style="width:12px;height:12px;border-radius:50%;background:#28c840"></i>
      <span style="position:absolute;left:0;right:0;text-align:center">${title}</span>
    </div>
    <img src="${pathToFileURL(raw).href}" style="display:block;width:${width}px">
  </div></body>`;
  const height = await imageHeight(raw, width);
  await page(name, html, width + 128, height + 38 + 112 + 2, dark);
}
async function imageHeight(raw, width) {
  await cdp("Page.navigate", { url: pathToFileURL(raw).href });
  await sleep(500);
  return js(`(() => { const i = document.querySelector("img"); return Math.round(i.naturalHeight / i.naturalWidth * ${width}); })()`);
}

async function banner(dark) {
  const t = theme(dark);
  const html = `<body style="width:1280px;height:400px;background:${t.bg};background-image:radial-gradient(45% 80% at 80% 50%,${t.glow},transparent 70%),radial-gradient(40% 60% at 0% 100%,${t.glow2},transparent 70%);display:flex;align-items:center;padding:0 96px;box-sizing:border-box;font-family:'Mona Sans',system-ui;color:${t.text};overflow:hidden;position:relative">
  <svg viewBox="0 0 24 24" style="position:absolute;right:120px;top:50%;width:300px;height:300px;transform:translateY(-50%);opacity:${dark ? ".9" : ".85"}"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#e9ff8a"/><stop offset="1" stop-color="#a8d62a"/></linearGradient></defs><path d="${SPARK}" fill="url(#g)"/></svg>
  <div>
    <div style="display:flex;align-items:center;gap:22px">
      <div style="width:84px;height:84px;border-radius:22px;background:#181a21;display:grid;place-items:center;box-shadow:0 10px 30px -10px rgba(0,0,0,.5)"><svg viewBox="0 0 24 24" width="50" height="50"><path d="${SPARK}" fill="#d6f554"/></svg></div>
      <div style="font-size:92px;font-weight:700;letter-spacing:-.035em;font-stretch:105%">Glint</div>
    </div>
    <div style="margin-top:22px;font-size:30px;line-height:1.35;color:${t.muted};max-width:900px;font-weight:450">A local debugging desk for you and your coding agent.</div>
  </div></body>`;
  await page(dark ? "banner-dark" : "banner-light", html, 1280, 400, dark);
}

await cdp("Page.enable");
await cdp("Runtime.enable");
await cdp("Emulation.setLocaleOverride", { locale: "en-US" });
const raw = (n) => join(work, `${n}.png`);
try {
  for (const dark of [true, false]) await banner(dark);

  for (const dark of [true, false]) {
    await open(base, { dark, height: 620 });
    await js("sendSample().then(() => new Promise((r) => setTimeout(r, 2000)))");
    await js(`(async () => {
      const li = [...document.querySelectorAll("li.entry")].find((r) => r.textContent.trim().startsWith("UnexpectedValueException"));
      (li?.querySelector("button:not(.twig)") || li)?.click();
      await new Promise((r) => setTimeout(r, 500));
      return true;
    })()`);
    await capture(raw("run"));
    await framed(dark ? "run-dark" : "run-light", raw("run"), { title: "Glint", dark });
  }

  await open(base, { dark: true, height: 820 });
  await js("openAgents(); true");
  await capture(raw("agents"));
  await framed("agents", raw("agents"), { title: "Glint · Agents" });

  await open(base, { dark: true, height: 820 });
  await js(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "?" })); true`);
  await capture(raw("docs"));
  await framed("docs", raw("docs"), { title: "Glint · Docs" });

  if (empty) {
    await open(empty, { dark: false, height: 860 });
    await capture(raw("first-run"));
    await framed("first-run", raw("first-run"), { title: "Glint", dark: false });
  }
} finally {
  ws.close();
  chrome.kill();
  await sleep(300);
  rmSync(work, { recursive: true, force: true });
}
