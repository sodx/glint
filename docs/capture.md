# Errors and frameworks with no code

## Uncaught errors

The global install chains each language's own handlers, so uncaught errors of PHP, Python and
Node processes reach Glint while output and exit codes stay as they were. A web front end in
development gets the same with one tag, which also reports `console.error`, failed requests and
markers sent with `window.glint()`:

```html
<script src="http://127.0.0.1:23600/v1/browser.js" data-project="shop"></script>
```

## Frameworks

Laravel, Django, Flask, FastAPI, Express, Next.js and WordPress need no package, middleware or
settings: the global install hooks into them when they load. Each request becomes a run with one
summary at its root (method, route, controller or view, status, time, memory, number of queries),
and under it every SQL query with the line in your code that ran it, the framework's exceptions
with their request, log records, and outgoing HTTP calls that failed.

| Framework | Hooked through | SQL from |
| --- | --- | --- |
| Laravel (also `artisan serve` and queue workers, one run per job) | the PHP prepend file, as the app boots | every connection (`DB::listen`) |
| Django | the Python shim, when the request handler loads | every connection (`execute_wrapper`) |
| Flask, FastAPI, Starlette | the Python shim | SQLAlchemy |
| Express, Next.js, any `node:http` server | the Node shim (`NODE_OPTIONS`) | `pg`, `mysql2`, `node:sqlite` |
| WordPress, Gutenberg included | the PHP prepend file, before WordPress loads | `$wpdb`, with the theme or plugin line behind each query |

WordPress also reports database errors, `WP_Error`s, `_doing_it_wrong()` and deprecation notices,
mail sent with `wp_mail()`, HTTP API calls and REST responses with an error status. The browser
script goes into the front end, wp-admin and the login page, and in the block editor invalid blocks
and notices such as "Updating failed" arrive with the reason. `Glint\WordPress` adds `post()`,
`user()`, `query()`, `hooks('save_post')`, `template()`, `conditionals()`, `options()` and
`queries()` for code. A production site is left alone, and `GLINT_WP` chooses what to report.

`sdks/js/src/vite.js` is a Vite plugin that puts the browser script on dev server pages and nothing
in a production build. `examples/frameworks/` holds a small app for each framework, and
`python3 examples/frameworks/check.py` runs the ones whose toolchain is installed against a sandbox
glintd and checks what arrives.

[Back to the README](../README.md)
