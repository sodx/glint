// Conformance program for glint-js. Run with: glint conformance -- node sdks/js/conformance.mjs
// Scenarios: spec/conformance/scenarios.json (same order).

import glint from "./src/index.js";

const begin = (scenario) => glint.send("x-conformance.begin", { scenario });

class ConformanceOrder {
  constructor() {
    this.id = 7;
    this.total = 129.5;
  }
}

function sleepSync(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

begin("scalars");
glint(42);
glint("hello");
glint(true);
glint(null);
glint(3.5);

begin("structured");
glint({ name: "Ada", tags: ["vip", "eu"], nested: { n: 1 } });

begin("object-type");
glint(new ConformanceOrder());

begin("cycle");
const a = { name: "a" };
a.self = a;
glint(a);

begin("special-numbers");
glint({ nan: NaN, inf: Infinity, ninf: -Infinity });

begin("depth");
let deep = {};
for (let i = 0; i < 50; i++) deep = { child: deep };
glint(deep);

begin("multiple-values");
glint(1, 2).label("pair");

begin("fluent");
glint("styled").color("red").label("L").large();

begin("hide-remove");
glint("h").hide();
glint("r").remove();

begin("log");
glint.log("user signed in", { user_id: 7 }, "warn");

begin("table");
glint.table([{ sku: "A-1", qty: 2 }, { sku: "B-7", qty: 1 }]);

begin("exception");
function failing() {
  throw new TypeError("boom");
}
try {
  failing();
} catch (err) {
  glint.exception(err);
}

begin("measure");
const got = glint.measure("work", () => {
  sleepSync(20);
  return 5;
});
if (got !== 5) {
  console.error(`measure returned ${got}, expected 5`);
  process.exitCode = 1;
}

begin("send");
glint.send("x-conformance.custom", { a: 1 }, { label: "raw" });

begin("origin");
glint("origin-check"); // glint:origin

begin("end");
glint("end");
