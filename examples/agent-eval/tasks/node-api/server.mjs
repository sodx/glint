// The cart API: GET /cart/<id>/total → {"cart": id, "total": n}.
import { createServer } from "node:http";
import { readFileSync } from "node:fs";

const carts = JSON.parse(readFileSync(new URL("./carts.json", import.meta.url)));
const port = Number(process.env.PORT || 8180);

function lineTotal(item) {
  return item.price.amount.toFixed(2) * item.qty;
}

function cartTotal(cart) {
  const items = cart.items.reduce((sum, item) => sum + lineTotal(item), 0);
  return Math.round((items + (cart.shipping?.amount ?? 0)) * 100) / 100;
}

createServer((req, res) => {
  const m = req.url.match(/^\/cart\/(\w+)\/total$/);
  if (!m || !carts[m[1]]) { res.writeHead(404).end(); return; }
  // Totals are computed off the request path, like the real service does with its pricing queue.
  setImmediate(() => {
    const total = cartTotal(carts[m[1]]);
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ cart: m[1], total }));
  });
}).listen(port, "127.0.0.1");
