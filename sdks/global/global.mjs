// Written by `glint install`: glint() as a global in every Node process on this machine, and
// uncaught errors reported to Glint without any code. Off in production like the SDK.
import glint from "./src/index.js";

if (typeof globalThis.glint === "undefined") globalThis.glint = glint;

// Uncaught exceptions and unhandled rejections: the monitor event changes nothing about how the
// process crashes; the report goes out synchronously, because the process exits right after.
if (typeof process !== "undefined" && process.on) {
  process.on("uncaughtExceptionMonitor", (err) => {
    try {
      glint.exception(err).label("uncaught");
      glint.flushSync();
    } catch {
      // a debugging aid never changes what the program does
    }
  });
}
