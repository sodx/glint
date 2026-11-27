"""A one-file FastAPI app for Glint's framework check (SQL through SQLAlchemy, outgoing calls
through httpx). Nothing in it mentions Glint: the global shim hooks in when FastAPI loads.

    python3 app.py 8783
"""
import logging
import sys

import httpx
import uvicorn
from fastapi import FastAPI
from sqlalchemy import create_engine, text
from sqlalchemy.pool import StaticPool

app = FastAPI()
db = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
log = logging.getLogger("shop")

with db.begin() as c:
    c.execute(text("create table orders (id integer primary key, customer text, total real)"))


@app.get("/orders/{customer}")
def orders(customer: str):
    with db.begin() as c:
        c.execute(text("insert into orders (customer, total) values (:c, 42.5)"), {"c": customer})
        rows = c.execute(text("select id, total from orders where customer = :c"), {"c": customer}).all()
    log.warning("customer %s has %d orders", customer, len(rows))
    return {"orders": len(rows)}


@app.get("/boom")
async def boom():
    order = {"id": 7}
    return {"email": order["email"]}  # KeyError


@app.get("/upstream")
async def upstream():
    async with httpx.AsyncClient() as client:
        await client.get("http://127.0.0.1:59999/unreachable", timeout=1)
    return {"ok": True}


if __name__ == "__main__":
    uvicorn.run(app, port=int(sys.argv[1]) if len(sys.argv) > 1 else 8783, log_level="warning")
