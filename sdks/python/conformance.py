"""Conformance program for glint-python.

Run from the repo root:  glint conformance -- python3 sdks/python/conformance.py
Scenarios: spec/conformance/scenarios.json (same order). The "end" scenario relies on the
SDK's atexit flush, so this file must not call glint.flush().
"""

import os
import sys
import time

# Make `import glint` resolve to the package next to this file, even under `python3 -I`.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from glint import glint  # noqa: E402


def begin(scenario):
    glint.send("x-conformance.begin", {"scenario": scenario})


class ConformanceOrder:
    def __init__(self):
        self.id = 7
        self.total = 129.5


class Node:
    def __init__(self, name):
        self.name = name


begin("scalars")
glint(42)
glint("hello")
glint(True)
glint(None)
glint(3.5)

begin("structured")
glint({"name": "Ada", "tags": ["vip", "eu"], "nested": {"n": 1}})

begin("object-type")
glint(ConformanceOrder())

begin("cycle")
a = Node("a")
a.self = a
glint(a)

begin("special-numbers")
glint({"nan": float("nan"), "inf": float("inf"), "ninf": float("-inf")})

begin("depth")
deep = {}
for _ in range(50):
    deep = {"child": deep}
glint(deep)

begin("multiple-values")
glint(1, 2).label("pair")

begin("fluent")
glint("styled").color("red").label("L").large()

begin("hide-remove")
glint("h").hide()
glint("r").remove()

begin("log")
glint.log("user signed in", {"user_id": 7}, "warn")

begin("table")
glint.table([{"sku": "A-1", "qty": 2}, {"sku": "B-7", "qty": 1}])


def failing():
    raise ValueError("boom")


begin("exception")
try:
    failing()
except ValueError as err:
    glint.exception(err)


def bang():
    raise ValueError("bang")


begin("value-exception")
try:
    bang()
except ValueError as err:
    glint(err)


def work():
    time.sleep(0.02)
    return 5


begin("measure")
got = glint.measure("work", work)
if got != 5:
    print(f"measure returned {got!r}, expected 5", file=sys.stderr)
    sys.exit(1)

begin("send")
glint.send("x-conformance.custom", {"a": 1}, {"label": "raw"})

begin("measure-nesting")
glint.measure("outer", lambda: (glint("in outer"), glint.measure("inner", lambda: glint("in inner"))))

begin("origin")
glint("origin-check")  # glint:origin

begin("origin-method")
glint.log("origin-log")  # glint:origin

begin("end")
glint("end")
