# Secrets

glintd replaces passwords, tokens and keys in every event before storing it, so they never reach
the database, the window or a coding agent. It catches values under secret-looking keys
(`password`, `api_key`, `authorization`, `cookie`, `*_token`, `dsn`, ...) at any depth, SQL
bindings for such columns, and well-known shapes anywhere in text: Bearer tokens, JWTs, AWS, GitHub,
Slack, Stripe, OpenAI and Anthropic keys, Google API keys, PEM private keys and `user:pass@` in
URLs. Values read `[redacted]`, or keep their prefix (`sk-ant-…[redacted]`), and the event's
`meta.redacted` says how many were replaced. Counters like `token_count` and ids like `session_id`
stay visible.

Settings, under Secrets, adds key names and regular expressions, lists keys never to hide, or
turns it off. In `~/.glint/config.toml` these are `redact`, `redact_keys`, `redact_patterns` and
`redact_allow` (environment: `GLINT_REDACT`, `GLINT_REDACT_KEYS`, ...).

[Back to the README](../README.md)
