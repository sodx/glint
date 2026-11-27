# glint/glint for PHP

Send values, logs, exceptions and timings from PHP to Glint, the local debug desk. The API follows
Ray's shape (a global `glint()` helper with a fluent chain), but Glint receives structured JSON,
never pre-rendered HTML.

Requires PHP 8.1+ and `ext-json`. `ext-curl` is recommended; without it the SDK uses
`file_get_contents` with a timeout. No other dependencies.

## Install

Until it is published, use a path repository in the project's `composer.json`:

```json
{
    "repositories": [{ "type": "path", "url": "../glint/sdks/php" }],
    "require": { "glint/glint": "@dev" }
}
```

Then `composer require glint/glint:@dev`. Composer loads `src/helpers.php`, which defines `glint()`.

## Usage

```php
glint($order)->label('before save')->orange();   // one value, styled
glint(1, 2)->label('pair');                      // several values, one call
glint(['name' => 'Ada'])->large()->hide();

Glint::log('user signed in', ['user_id' => 7], 'warn');
Glint::table($rows, ['sku', 'qty']);
Glint::exception($e);                            // class, message, frames, cause
$result = Glint::measure('import', fn () => import());   // returns fn's result
Glint::send('x-custom.kind', ['a' => 1], ['label' => 'raw']);
Glint::flush(2.0);                               // optional: send now and wait
```

Objects keep their fields (including private and protected ones) and report their class in
`payload.type`. Cycles become `{"$ref": ...}`, binary strings `{"$bin": ...}`, NaN and infinities
the strings `"NaN"`, `"Infinity"`, `"-Infinity"`. Nesting deeper than 10 levels becomes
`{"$truncated": true}`.

## Pause

```php
use Glint\Glint;
use Glint\GlintStop;

$answer = Glint::pause("refund {$order->total} EUR?");
if ($answer['approve'] ?? false) {
    $gateway->refund($order);
}
```

`pause` blocks until someone presses Continue in the Glint window, or a coding agent answers through
MCP, and returns the value they gave (or `null`). Stop throws `GlintStop`. It returns at once when
Glint is off or unreachable, and on CI unless `GLINT_FORCE_PAUSE` is set. After 5 minutes it gives
up and carries on. What `measure`'s callback sends nests under the measure, so the window shows the
request as a call tree.

## Configuration

| Variable | Default | Meaning |
|----------|---------|---------|
| `GLINT_ENABLED` | see below | `0`/`false`/`off` disables everything (no network); `1`/`true`/`on` enables |
| `GLINT_URL` | `http://127.0.0.1:23600` | Glint server |
| `GLINT_PROJECT` | unset | project name attached to every event |

Variables are read with `getenv()`, then `$_ENV` and `$_SERVER`, so a `.env` loader that fills
any of them works.

Without `GLINT_ENABLED`, the SDK is enabled unless `APP_ENV=production`. Set `GLINT_ENABLED=1`
to force it on in production.

Programmatically: `Glint::configure(url: 'http://127.0.0.1:23600', enabled: true, project: 'shop')`.

## Laravel

The `APP_ENV` default covers production. Laravel fills `$_ENV` from `.env`, so nothing else is
needed. Add `GLINT_ENABLED=1` to `.env` only when you want it on in production.

## When Glint is down

Events are queued in memory and sent in batches of 100, when 100 ms has passed since the last
send (checked on each call), and at shutdown. The connection timeout is 500 ms and the request
timeout 2 s. After a failed send the SDK stops trying for 5 s and drops events meanwhile. The
drop count is reported as `meta.dropped` on the next event that gets through. The queue holds at
most 10 000 events; beyond that, the oldest are dropped. Your program never sees an exception
from the SDK.

## Conformance

```bash
cargo build -q -p glint-cli
./target/debug/glint conformance -- php sdks/php/conformance.php
```
