// Written by `glint install`: glint() as a global in every Node process on this machine, and
// uncaught errors reported to Glint without any code. Off in production like the SDK.
//
// Node loads this file through NODE_OPTIONS=--import for every process, so nothing here may ever
// throw: a missing or broken SDK copy (a deleted ~/.glint, a half-written update) leaves the
// program running exactly as it would without Glint. Hence the dynamic imports inside try.
try {
  const { default: glint } = await import("./src/index.js");

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

  // Frameworks, with no code: every HTTP server reports a run per request, Express errors, failed
  // fetch() calls and the SQL of pg, mysql2 and node:sqlite (src/frameworks.js).
  try {
    const { install } = await import("./src/frameworks.js");
    await install();
  } catch {
    // never stop the program from starting
  }
} catch {
  // never stop the program from starting
}
