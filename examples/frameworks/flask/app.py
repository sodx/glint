"""A one-file Flask app for Glint's framework check (SQL through SQLAlchemy). Nothing in it
mentions Glint: the global shim hooks in when Flask loads.

    python3 app.py 8782
"""
import logging
import sys

from flask import Flask, jsonify
from sqlalchemy import create_engine, text
from sqlalchemy.pool import StaticPool

app = Flask(__name__)
db = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
log = logging.getLogger("shop")

with db.begin() as c:
    c.execute(text("create table orders (id integer primary key, customer text, total real)"))


@app.get("/orders/<customer>")
def orders(customer):
    with db.begin() as c:
        c.execute(text("insert into orders (customer, total) values (:c, 42.5)"), {"c": customer})
        rows = c.execute(text("select id, total from orders where customer = :c"), {"c": customer}).all()
    log.warning("customer %s has %d orders", customer, len(rows))
    return jsonify(orders=len(rows))


@app.get("/boom")
def boom():
    order = {"id": 7}
    return jsonify(email=order["email"])  # KeyError


@app.get("/upstream")
def upstream():
    import requests

    requests.get("http://127.0.0.1:59999/unreachable", timeout=1)
    return jsonify(ok=True)


if __name__ == "__main__":
    app.run(port=int(sys.argv[1]) if len(sys.argv) > 1 else 8782)
