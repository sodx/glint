# Does Glint help a coding agent?

A small A/B experiment. Claude Code fixes a few seeded bugs twice, once with Glint and once
without, and the run records turns, time, cost and whether the fix passed its check.

It costs money and is never run by the test suites. Start with a dry run:

```bash
python3 examples/agent-eval/run.py --dry-run
```

## What it does

Each folder in `tasks/` is a small program with one bug, a prompt (`task.json`) and a check command
that exits 0 when the bug is fixed:

| Task | Program | What goes wrong |
|---|---|---|
| `py-worker` | Python batch job | one order is skipped by a silent `except` |
| `node-api` | Node HTTP API, started by its check | one request crashes the server; its output is not shown |
| `php-shop` | PHP under `php -S` with `display_errors=0` | one page is an empty HTTP 500 |

For every trial the task is copied to a fresh folder with its own git repository, and
`claude -p` gets the prompt in one of two conditions:

- `glint`: Glint's MCP server, the Claude Code hooks (context, stop guard) and `glint()` in the
  programs, all pointing at a private glintd.
- `plain`: no MCP server (`--strict-mcp-config`), no hooks (`--setting-sources project,local`
  keeps your own settings out), and `GLINT_ENABLED=0` for the programs.

Both get the same prompt, tools (`Bash, Read, Edit, Write, Glob, Grep`) and permission mode
(`acceptEdits`). After the agent finishes, the check runs with Glint turned off.

The run starts its own glintd on port 23799 with a database and HOME under the output folder, so
your events, settings and hook state are not touched. `glint()` in the programs needs
`glint install` to have been run once on this machine; without it the `glint` condition still
has MCP and hooks but no events from the programs.

## Running it

Build glintd and glint first (`cargo build --release -p glintd -p glint-cli`), then:

```bash
python3 examples/agent-eval/run.py --yes --trials 3
```

```bash
python3 examples/agent-eval/run.py --yes --tasks node-api --conditions glint --model sonnet
```

Results go to `examples/agent-eval/out/<time>/results.jsonl`, one line per trial: task,
condition, pass, turns, seconds, cost and, for the `glint` condition, how the agent used Glint
(tool calls by tool, hooks, failures read and how soon). A table per condition is printed at the
end. The agents' folders stay in `out/<time>/work/` to read what they changed. To look at the
recorded agent activity, start the private glintd again with the command the run prints and press
`g` in its window.

## Cost

Every trial is one headless Claude Code session. On these tasks a session usually takes 5 to 25
turns and costs about $0.05 to $0.50, depending on the model and how lost the agent gets. One
full round is 3 tasks x 2 conditions = 6 sessions, so roughly $0.30 to $3 per trial; `--trials 3`
is roughly $1 to $9. The real figure is in `cost_usd` of every result line (Claude Code's
`total_cost_usd`). With a subscription login instead of an API key the sessions count against your
plan's usage instead.

## Reading the result

Three tasks and a few trials say little on their own: agents vary a lot between runs. Look at the
pass rate and median turns per condition across at least 3 trials, and at `glint.calls` to see
whether the agent used Glint at all. A task where the `glint` condition never calls a tool says
more about the prompt and the instructions than about Glint.

## Adding a task

Make a folder in `tasks/` with the program, a `task.json` (`prompt`, `needs`: commands that must
be on PATH, `check`: the command that exits 0 when fixed) and a check that hides the program's
own output the way production would. Keep the bug reproducible and the prompt free of hints.
