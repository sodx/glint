// Vite plugin: puts Glint's browser script on the dev server's pages, so the browser's errors,
// console.error, failed requests and window.glint() markers land in Glint. Does nothing in a
// production build (`vite build`) or when Glint is off.
//
//   // vite.config.js
//   import glint from "glint-sdk/vite";
//   export default { plugins: [glint()] };
//
// Options: project (default: GLINT_PROJECT, else the git repository's name, else the folder's),
// url (default: GLINT_URL, else http://127.0.0.1:23600).

import { existsSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";

/** The project name glintd gives this folder: the git repository's, else the folder's. */
export function projectName(root) {
  let dir = resolve(root);
  for (;;) {
    const git = join(dir, ".git");
    if (existsSync(git)) {
      try {
        if (statSync(git).isFile()) {
          const m = readFileSync(git, "utf8").match(/gitdir:\s*(.+)/);
          const wt = m && m[1].trim().match(/^(.*)[\\/]\.git[\\/]worktrees[\\/]/);
          if (wt) return basename(wt[1]);
        }
      } catch {
        // fall through to the folder name
      }
      return basename(dir);
    }
    const up = dirname(dir);
    if (up === dir) return basename(resolve(root));
    dir = up;
  }
}

function enabled() {
  const v = String(process.env.GLINT_ENABLED || "").toLowerCase();
  if (["0", "false", "off"].includes(v)) return false;
  return true;
}

export default function glint(options = {}) {
  let root = process.cwd();
  return {
    name: "glint",
    apply: "serve", // dev server only; `vite build` never sees it
    configResolved(config) {
      root = config.root || root;
    },
    transformIndexHtml() {
      if (!enabled()) return [];
      const url = String(options.url || process.env.GLINT_URL || "http://127.0.0.1:23600").replace(/\/$/, "");
      const project = options.project || process.env.GLINT_PROJECT || projectName(root);
      return [
        {
          tag: "script",
          attrs: { src: `${url}/v1/browser.js`, "data-project": project, crossorigin: "anonymous" },
          injectTo: "head-prepend",
        },
      ];
    },
  };
}
