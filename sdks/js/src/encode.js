// Turns any JavaScript value into JSON that says what the value was.
// Rules: spec/conformance/README.md, "Requirements" 4.

export const MAX_DEPTH = 10;
const MAX_ITEMS = 1000;
const MAX_STRING = 100_000;

/** Source type name for a top-level value, or undefined for plain data. */
export function typeName(v) {
  if (v === null || typeof v !== "object") return undefined;
  const name = v.constructor && v.constructor.name;
  if (!name || name === "Object" || name === "Array") return undefined;
  return name;
}

export function encode(value, maxDepth = MAX_DEPTH) {
  const ancestors = new Map(); // object -> JSON path, only along the current branch

  function walk(v, depth, path) {
    switch (typeof v) {
      case "undefined":
        return null;
      case "number":
        if (Number.isNaN(v)) return "NaN";
        if (v === Infinity) return "Infinity";
        if (v === -Infinity) return "-Infinity";
        return v;
      case "bigint":
        return v.toString();
      case "string":
        return v.length > MAX_STRING ? v.slice(0, MAX_STRING) + "…" : v;
      case "boolean":
        return v;
      case "symbol":
        return v.toString();
      case "function":
        return `[Function ${v.name || "anonymous"}]`;
    }
    if (v === null) return null;
    if (ancestors.has(v)) return { $ref: ancestors.get(v) };
    if (depth >= maxDepth) return { $truncated: true };

    if (v instanceof Date) return Number.isNaN(v.getTime()) ? "Invalid Date" : v.toISOString();
    if (v instanceof RegExp) return String(v);
    if (typeof ArrayBuffer !== "undefined" && (v instanceof ArrayBuffer || ArrayBuffer.isView(v)) && !(v instanceof DataView)) {
      const bytes = v instanceof ArrayBuffer ? new Uint8Array(v) : new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
      return { $bin: base64(bytes.length > MAX_STRING ? bytes.subarray(0, MAX_STRING) : bytes) };
    }

    ancestors.set(v, path);
    try {
      if (v instanceof Error) {
        return { class: v.name, message: v.message };
      }
      if (Array.isArray(v) || v instanceof Set) {
        const items = Array.from(v);
        const out = items.slice(0, MAX_ITEMS).map((x, i) => walk(x, depth + 1, `${path}[${i}]`));
        if (items.length > MAX_ITEMS) out.push({ $truncated: true, more: items.length - MAX_ITEMS });
        return out;
      }
      const out = {};
      const entries = v instanceof Map ? Array.from(v.entries()) : Object.keys(v).map((k) => [k, safeGet(v, k)]);
      let n = 0;
      for (const [k, x] of entries) {
        if (n++ >= MAX_ITEMS) {
          out.$truncated = true;
          break;
        }
        const key = typeof k === "string" ? k : String(k);
        out[key] = walk(x, depth + 1, `${path}.${key}`);
      }
      return out;
    } finally {
      ancestors.delete(v);
    }
  }

  return walk(value, 0, "$");
}

function safeGet(obj, key) {
  try {
    return obj[key];
  } catch (e) {
    return `[threw: ${e && e.message}]`;
  }
}

function base64(bytes) {
  if (typeof Buffer !== "undefined") return Buffer.from(bytes).toString("base64");
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}
