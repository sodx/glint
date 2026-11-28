# Coding agents

`glint install` registers Glint's MCP server with the agents it finds, by their settings folder,
their app or their command. Claude Code gets it at user scope, so every project sees it, plus hooks
that put its own session next to your program's events. Cursor, VS Code, Windsurf and Gemini CLI
get the server over HTTP, and Codex over stdio. Existing settings are kept. Before each change Glint
keeps a copy named `<file>.glint-backup-<time>` (the newest five per file), and it replaces a file
in one step, so an agent never reads half of one. A file that exists but cannot be read, or is not
plain JSON, is left alone with a hint. For Claude Code, Glint adds the new server entry before it
removes an old one, and puts the old one back if the new one fails. `glint uninstall` removes
exactly what was added. An agent loads Glint when a session starts, so start a new one after setup.

## Which project the tools read

Every tool that takes a `project` reads the project the agent works in, and every answer starts
with a line naming it and how Glint knows:

- `glint mcp`, the stdio server Codex uses, sends the project of the folder it runs in.
- Claude Code, VS Code and Cursor connect over HTTP and say nothing about their folder up front.
  On the first tool call of a session Glint asks them for their workspace folder (the MCP `roots`
  request) and uses that folder's git repository as the project.
- For a client that answers neither, register the server as `http://127.0.0.1:23600/mcp?project=shop`,
  or pass `project` to the tool.

Passing `project` always wins, and `project="*"` reads every project. When Glint cannot tell the
project and several have events, a tool does not mix them: it says so and lists the recent projects.

`glint_wait_for` first looks at the last 10 seconds (`lookback_s`, or everything after an event id
with `after`), so a program the agent ran just before asking counts. Its answer says which window
it searched.

## What the hooks tell Claude Code

The hooks also let Glint talk to Claude Code. At the start of a session it learns that Glint
records the project, how `glint()` works and which tools to use. When glintd is not running it
learns the same, plus that Glint is off and how to start it. With each of your prompts it gets the
failures that happened since the previous one, so when you write "it's broken" it already has the
error and the line. A list shows the newest five failures and says how many more there are.

When it wants to finish a turn during which your programs failed, the Stop guard sends it back once
to fix them or say why they are expected. A failure in a file it edited afterwards is left out only
when a later run of the same program passed; otherwise it is listed as possibly fixed. If the
session's start was never marked (Glint was down then), the guard starts counting from that point
on instead of staying off. Both behaviours can be turned off in Settings. The hook command quotes
its path, so a home folder with spaces works.

## The Agents view

<img alt="The Agents view: tool calls by Claude Code and Cursor, which tools they used, and how soon a failure reached them" src="assets/agents.png">

The Agents view (`g`) shows what agents did with Glint: which ones connected, which tools they
called, how many failures they read and how soon after the failure happened. Inside a run, events
an agent read carry a "read by" mark. `glint stats` prints the same numbers. All of it is recorded
on this machine and kept as long as events. `examples/agent-eval/` is an opt-in experiment that
runs seeded bugs with and without Glint, to measure whether it helps; it costs model usage, so it
never runs in tests.

To connect an agent by hand:

```bash
claude mcp add --transport http glint http://127.0.0.1:23600/mcp   # or: claude mcp add glint -- glint mcp
glint hook --print-config                                          # Claude Code hooks for .claude/settings.json
```

[Back to the README](../README.md)
