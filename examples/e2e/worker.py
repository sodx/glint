"""A queue worker debugged with the global glint() that `glint install` provides: no import.

    python3 examples/e2e/worker.py
"""
import random
import time

JOBS = [
    {"id": 1, "type": "send_invoice", "order": 7, "email": "ada@example.com"},
    {"id": 2, "type": "resize_image", "path": "products/a-1.png", "sizes": [64, 256, 1024]},
    {"id": 3, "type": "send_invoice", "order": 8},  # no email: the bug
    {"id": 4, "type": "sync_stock", "skus": {"A-1": 12, "B-7": 0, "C-3": 4}},
]


def send_invoice(job):
    to = job["email"]  # KeyError for job 3
    glint.log("invoice sent", {"order": job["order"], "to": to})


def resize_image(job):
    for size in job["sizes"]:
        time.sleep(0.01)
    glint.log("resized", {"path": job["path"], "sizes": job["sizes"]})


def sync_stock(job):
    empty = [sku for sku, n in job["skus"].items() if n == 0]
    if empty:
        glint.log("out of stock", {"skus": empty}, level="warn")
    glint(job["skus"]).label("stock after sync")


HANDLERS = {"send_invoice": send_invoice, "resize_image": resize_image, "sync_stock": sync_stock}

results = []
glint.log("worker started", {"queue": "default", "jobs": len(JOBS)})
for job in JOBS:
    glint(job).label(f"job {job['id']}")
    started = time.time()
    try:
        glint.measure(job["type"], lambda: HANDLERS[job["type"]](job))
        results.append({"job": job["id"], "type": job["type"], "status": "done", "ms": round((time.time() - started) * 1000, 1)})
    except Exception as err:
        glint(err)
        results.append({"job": job["id"], "type": job["type"], "status": "failed", "ms": round((time.time() - started) * 1000, 1)})

glint.table(results).label("run summary")
glint.flush()
