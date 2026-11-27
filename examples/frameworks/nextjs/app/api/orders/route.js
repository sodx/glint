import { DatabaseSync } from "node:sqlite";

const db = new DatabaseSync(":memory:");
db.exec("create table orders (id integer primary key, customer text, total real)");

export function GET(request) {
  const customer = new URL(request.url).searchParams.get("customer") || "ada";
  db.prepare("insert into orders (customer, total) values (?, ?)").run(customer, 42.5);
  const rows = db.prepare("select id, total from orders where customer = ?").all(customer);
  return Response.json({ orders: rows.length });
}
