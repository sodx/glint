// Exit 0 when the API answers all three totals right. The server's own output is not shown,
// as in production where it runs as a service.
import { spawn } from "node:child_process";

const port = 8180 + Math.floor(Math.random() * 500);
const srv = spawn(process.execPath, ["server.mjs"], { env: { ...process.env, PORT: String(port) }, stdio: "ignore" });
const want = { c1: 16, c2: 17.4, c3: 25.5 };
await new Promise((r) => setTimeout(r, 400));
let failed = 0;
for (const [id, total] of Object.entries(want)) {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/cart/${id}/total`, { signal: AbortSignal.timeout(3000) });
    const body = await r.json();
    if (Math.abs(body.total - total) > 0.001) { console.log(`${id}: got ${body.total}, want ${total}`); failed++; }
    else console.log(`${id}: ok`);
  } catch (e) {
    console.log(`${id}: no answer (${e.cause?.code || e.name})`);
    failed++;
  }
}
srv.kill();
process.exit(failed ? 1 : 0);
