// A Node HTTP API debugged with the global glint() that `glint install` provides: no import.
//   node examples/e2e/api.mjs        (port 8092; GET /users/7, POST /refunds/7, GET /report)
//
// POST /refunds/:id stops on glint.pause() until someone answers in Glint (UI, MCP or API);
// the answer's value decides the refund.
import http from "node:http";

const users = { 7: { id: 7, name: "Ada Lovelace", plan: "pro", tags: ["vip", "eu"], billing: { currency: "EUR", card: { brand: "visa", last4: "4242" } } } };

function catalogue() {
  // A big value, to see folding: 60 products with nested prices.
  return Array.from({ length: 60 }, (_, i) => ({ sku: `P-${i + 1}`, name: `Product ${i + 1}`, price: { amount: 10 + i, currency: "EUR" }, stock: { warehouse: i % 7, shop: i % 3 } }));
}

const server = http.createServer(async (req, res) => {
  glint.log(`${req.method} ${req.url}`);
  try {
    let m;
    if (req.method === "GET" && (m = req.url.match(/^\/users\/(\d+)$/))) {
      const user = users[m[1]];
      glint(user).label("user");
      return res.end(JSON.stringify(user) + "\n");
    }
    if (req.method === "POST" && (m = req.url.match(/^\/refunds\/(\d+)$/))) {
      const order = { id: Number(m[1]), amount: 129.5, reason: "damaged", customer: users[7] };
      glint(order).label("refund request");
      const answer = await glint.pause(`refund ${order.amount} EUR for order ${order.id}?`, { timeoutMs: 120_000 });
      glint({ answer }).label("refund decision");
      return res.end(answer?.approve ? `refunded ${order.amount}\n` : "refund declined\n");
    }
    if (req.method === "GET" && req.url === "/report") {
      const items = catalogue();
      glint(items).label("catalogue");
      const totals = items.reduce((t, p) => t + p.price.amount * p.stock.shop, 0);
      return res.end(`stock value ${totals.toFixed(2)} ${items[0].price.currency.toLowerCase().missing.x}\n`); // bug: TypeError
    }
    res.statusCode = 404;
    res.end("not found\n");
  } catch (err) {
    glint(err);
    res.statusCode = 500;
    res.end(`500: ${err.message}\n`);
  }
});
server.listen(Number(process.env.PORT || 8092), "127.0.0.1", () => glint.log("api listening", { port: Number(process.env.PORT || 8092) }));
