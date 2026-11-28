# Secrets

glintd replaces passwords, tokens and keys in every event before storing it, so they never reach
the database, the window or a coding agent. It catches values under secret-looking keys
(`password`, `api_key`, `authorization`, `cookie`, `*_token`, `dsn`, ...) at any depth, SQL
bindings for such columns, and well-known shapes anywhere in text: Bearer tokens, JWTs, AWS, GitHub,
Slack, Stripe, OpenAI and Anthropic keys, Google API keys, PEM private keys and `user:pass@` in
URLs. Values read `[redacted]`, or keep their prefix (`sk-ant-…[redacted]`), and the event's
`meta.redacted` says how many were replaced. Counters like `token_count` and ids like `session_id`
stay visible.

Payment and identity data is caught too:

- Card numbers: keys such as `card_number`, `cc_number`, `cvv`, `cvc`, and any 13 to 19 digit
  number in text that passes the Luhn check (`4111 1111 1111 1111`). A bare `card` or `cc` key is
  hidden only when its value looks like a card number, so a Stripe `card` object (brand, last4)
  and the `cc` list of an email stay readable.
- `ssn`, `otp`, `pin` and `pin_code` as the last word of a key (`user_pin`, `userPin`), never
  inside another word (`shipping`, `pinned`).
- Sessions and request forgery tokens: `session`, `PHPSESSID`, `csrf`, `xsrf`, also as
  `session=...` or `csrf_token: ...` in text. `session_id` stays.
- Signatures: `sig` and `signature` values (`X-Amz-Signature=...`, `?sig=...` in URLs), while
  `signature_method` and similar descriptions stay.

Settings, under Secrets, adds key names and regular expressions, lists keys never to hide, or
turns it off. In `~/.glint/config.toml` these are `redact`, `redact_keys`, `redact_patterns` and
`redact_allow` (environment: `GLINT_REDACT`, `GLINT_REDACT_KEYS`, ...).

[Back to the README](../README.md)
