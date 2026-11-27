# Framework integrations

The global SDK shims that `glint install` sets up (`sdks/global/`) hook into web frameworks when
they load, with no package, middleware or settings in the app. They emit existing kinds only.
This page is their manifest: what each one sends and how it gets in.

## The request

Every integration gives each request a run of its own (`run` is new per request, also in long
lived Python and Node processes) and one root event, sent when the response is done:

| Field | Value |
| --- | --- |
| `kind` | `core.measure` |
| `payload.name` | `GET /orders/{customer} → 500 · OrderController@index` |
| `payload.duration_ms`, `memory_bytes` | wall time of the request, peak memory of the process |
| `payload.method`, `url`, `route`, `status`, `queries`, `sql_ms` | the request; `route` is the pattern, not the path |
| `payload.action`, `view`, `views` | Laravel controller action and Blade views; Django and Flask view; FastAPI endpoint |
| `meta.label` | `Laravel`, `Laravel job`, `Django`, `Flask`, `FastAPI`, `Starlette`, `Express`, `Next.js`, `Node HTTP` |
| `meta.level` | `error` when the status is 5xx or a job failed |
| `origin` | the controller method, view function or job `handle()` when known |

Everything sent while the request runs (SQL, logs, exceptions, `glint()` markers, `measure()`)
names the root as `parent`, so the UI shows the request as a call tree.

## Other events

| Kind | When | Payload extras |
| --- | --- | --- |
| `sql.query` | every query | `sql`, `bindings`, `duration_ms`, `rows`, `conn`; `error` and `meta.level: error` when it failed; Laravel adds `caller`. `origin` is the first frame of the app's own code (outside `vendor/`, `site-packages`, `node_modules`). |
| `core.exception` | an exception the framework catches | `context`: method, url, route, view or action, input (Laravel, secrets hidden), job |
| `core.log` | Laravel `Log::`; Python `logging` records (INFO and up of the app's loggers, WARNING and up of the frameworks') | `fields`, `logger` |
| `http.request` | an outgoing call that failed: connection error, 4xx, 5xx | `method`, `url`, `status`, `error`, `duration_ms`; `meta.level` is `warn` for 4xx, else `error` |

## How each one gets in

| Framework | Entry | Hooks |
| --- | --- | --- |
| Laravel | `autoload.php` finds `artisan` and `vendor/laravel/framework` above the script; `src/laravel-env.php` defines `env()` before Laravel does, and its first call attaches to the application | events `QueryExecuted`, `MessageLogged`, `RouteMatched`, `RequestHandled`, `composing: *`, HTTP client `ResponseReceived` and `ConnectionFailed`, queue `JobProcessing`, `JobProcessed`, `JobFailed`; `reportable()` on the exception handler. `artisan serve` gets a router that loads the prepend file, which PHP skips for router scripts. With a cached configuration, `LaravelServiceProvider` (Composer package discovery) attaches instead. |
| Django | `sys.meta_path` hook on `django.core.handlers.base` | `BaseHandler.get_response` and `get_response_async`; `connection_created` adds an `execute_wrapper`; `got_request_exception` |
| Flask | hook on `flask` | `Flask.wsgi_app`, `full_dispatch_request` (route), `handle_exception` |
| FastAPI, Starlette | hook on `starlette.applications` | `Starlette.__call__` for HTTP scopes |
| SQLAlchemy | hook on `sqlalchemy` | `Engine` events `before_cursor_execute`, `after_cursor_execute`, `handle_error` |
| requests, httpx | hooks on `requests`, `httpx` | `Session.send`, `Client.send`, `AsyncClient.send` |
| Node servers | `global.mjs` | `emit("request")` of `http.Server` and `https.Server`, in an `AsyncLocalStorage` context; an Express app gets an error middleware before its first error handler; `console.error(err)` during a request reports `err` (Next.js prints route errors this way) |
| fetch | `global.mjs` | `globalThis.fetch` |
| pg, mysql2, node:sqlite | `module.registerHooks` resolve hook (`Module._load` on older Node) | `Client.prototype.query`; `Connection.prototype.query` and `execute`; `StatementSync` `run`, `get`, `all` and `DatabaseSync.exec`. Pools pass the caller's line through the async context. |
| Vite | `glint-sdk/vite` plugin in `vite.config.js` | `transformIndexHtml` on the dev server only (`apply: "serve"`) |

Switches: `GLINT_LARAVEL=off`, `GLINT_FRAMEWORKS=off` (Python and Node), and the SDK's own
`GLINT_ENABLED`, `APP_ENV=production` and `NODE_ENV=production`.

## Checking them

`python3 examples/frameworks/check.py [names]` starts a sandbox glintd, runs each example app in
`examples/frameworks/` with the SDKs from this checkout, sends it requests and checks the events
above. A framework whose toolchain is missing is skipped with the reason.
