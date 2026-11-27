# Mapping: Claude Code hooks to Glint

Adapter: `crates/glint-ingest/src/agent/claude_code.rs`. Endpoint: `POST /v1/hooks/claude-code`
(raw hook JSON). Sender: `glint hook`. It prints nothing, because Claude Code reads hook stdout.
It always exits 0 and gives up after about 1 s, so it never slows or steers the agent.

## Install

```bash
glint hook --print-config        # paste into .claude/settings.json (project) or ~/.claude/settings.json
```

## Verified

On 2026-11-27 with Claude Code 2.1.317, `SessionStart`, `UserPromptSubmit` and `SessionEnd` from a
real session arrived and decoded. The shapes of `PermissionRequest`, `SubagentStop`, `SessionEnd`,
`UserPromptSubmit` and `UserPromptExpansion` also match payloads recorded from 2.1.281. Tool
hooks (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`) follow the documented shape and are
covered by tests, but have not yet been seen from a live session.

## Envelope fields

| Hook | Glint |
|------|-------|
| `session_id` | `run = "claude-code:<session_id>"`, `payload.session` |
| `cwd` | `project` = last path segment, `origin.cwd` |
| — | `origin.lang = "agent"`, `origin.framework = "claude-code"`, `origin.sdk = "glint-hooks/<v>"` |
| `agent_id`, `agent_type` (subagents) | copied into the payload |
| `transcript_path`, `scratchpad_dir` | dropped |

## Events

| Hook | Kind | Notes |
|------|------|-------|
| `SessionStart` / `SessionEnd` | `agent.session` | `status` started / ended, `source`, `reason`, `model` |
| `UserPromptSubmit` | `agent.step` | `input` = prompt (≤ 4 KB), blue |
| `Stop` / `SubagentStop` | `agent.step` | `status: stopped`, `output` = last assistant message |
| `SubagentStart` | `agent.step` | labelled "<agent_type> started" |
| `PreToolUse` | `agent.tool_call` | id `cc-<session>-<tool_use_id>`, `tool`, `args` (strings ≤ 4 KB) |
| `PostToolUse` / `PostToolUseFailure` | `agent.tool_result` + `core.update` | `parent` = the call; `ok`, `result`, `error`; the call turns green or red |
| `PermissionRequest` | `agent.permission` | level warn, orange: the agent is waiting for the human |
| `Notification` | `core.log` | |
| `PreCompact` / `PostCompact` | `core.log` | labelled "compact" |
| anything else | `x-claude-code.<snake_case>` | payload kept minus paths |

Prompts and tool inputs and outputs are stored locally, like any other event. Nothing leaves the
machine. Agents reading Glint over MCP see these events in the same timeline as the program's
own output.

## What the hooks record about themselves

`glint hook` reports its own actions to `POST /v1/agents/activity`, so `glint stats` and the
Agents view can say whether Glint reached the agent:

| Action | When | `detail` |
|---|---|---|
| `hook.context` | SessionStart, or UserPromptSubmit with new failures | `{event, failures}`; `events` lists the failures shown |
| `hook.stop_guard` | Stop sent the agent back | `{failures}`; `events` lists them |
| `hook.stop_outcome` | the next Stop after a guard | `{outcome: fixed}` when a file a failure came from changed since, else `explained` |
