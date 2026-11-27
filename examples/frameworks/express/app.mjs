// An Express app for Glint's framework check. Nothing in it mentions Glint: the global shim
// (NODE_OPTIONS=--import=.../global.mjs) hooks in.
//
//   node app.mjs 8785          PG_URL=postgres://… adds /pg/:customer
import express from "express";
import { DatabaseSync } from "node:sqlite";

const db = new DatabaseSync(":memory:");
db.exec("create table orders (id integer primary key, customer text, total real)");

const app = express();

app.get("/orders/:customer", (req, res) => {
  db.prepare("insert into orders (customer, total) values (?, ?)").run(req.params.customer, 42.5);
  const rows = db.prepare("select id, total from orders where customer = ?").all(req.params.customer);
  res.json({ orders: rows.length });
});

app.get("/boom", (req, res) => {
  const order = { id: 7 };
  res.json({ avatar: order.customer.avatar }); // TypeError
});

app.get("/upstream", async (req, res, next) => {
  try {
    await fetch("http://127.0.0.1:59999/unreachable");
    res.json({ ok: true });
  } catch (err) {
    next(err);
  }
});

if (process.env.PG_URL) {
  const { default: pg } = await import("pg");
  const pool = new pg.Pool({ connectionString: process.env.PG_URL });
  app.get("/pg/:customer", async (req, res) => {
    const r = await pool.query("select $1::text as customer, now() as at", [req.params.customer]);
    res.json(r.rows[0]);
  });
}

// The app's own error handler: Glint sees the error before it.
app.use((err, req, res, next) => {
  res.status(500).json({ error: err.message });
});

app.listen(Number(process.argv[2] || 8785));
