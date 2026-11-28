"""Builds the Docs view of the Glint window and writes it into ui/index.html.

    python3 ui/docs/build.py

Edit the text here, not in index.html: everything between the `docs:start` and `docs:end`
markers there is replaced. The method reference is one table: each method has a short
description and a real example for every language (PHP, Python, Node, browser, HTTP for any other
language). Code blocks carry a data-lang; the window highlights them when the docs open.

Writing rules for this file: sentence-case headings, plain active sentences, no em dashes, no
decorative bold, and say what happens rather than how good it is.
"""

import html
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]
INDEX = ROOT / "ui" / "index.html"

LANGS = [("php", "PHP"), ("python", "Python"), ("node", "Node"), ("browser", "Browser"), ("http", "HTTP")]
CODE_LANG = {"php": "php", "python": "python", "node": "js", "browser": "js", "http": "bash"}


def code(src, lang):
    return f'<pre class="code"><code data-lang="{lang}">{html.escape(src.strip(chr(10)))}</code></pre>'


def cmd(src, lang="bash"):
    return (
        f'<pre class="code cmd"><code data-lang="{lang}">{html.escape(src.strip())}</code>'
        '<button class="cc" data-cc aria-label="Copy"><svg class="i"><use href="#i-copy"/></svg></button></pre>'
    )


# ---------------------------------------------------------------- the method reference
# (id, title, description, {lang: example or None}, note shown under the tabs)
METHODS = [
    (
        "m-value",
        "glint(value): send a value",
        "Sends one event per value. Arrays, dictionaries, objects and models arrive as foldable JSON; an "
        "exception passed this way is shown as an exception with its stack. It returns a handle for "
        "the methods below.",
        {
            "php": """
$order = Order::with('items')->find($id);
glint($order);                       // one value
glint($request->all(), $order->total); // several, one event each
""",
            "python": """
order = db.orders.get(order_id)
glint(order)                         # one value
glint(request.json, order.total)     # several, one event each
""",
            "node": """
const order = await db.orders.find(orderId);
glint(order);                        // one value
glint(req.body, order.total);        // several, one event each
""",
            "browser": """
const cart = await fetch("/api/cart").then((r) => r.json());
glint(cart);
""",
            "http": """
curl -s localhost:23600/v1/events \\
  -d '{"kind":"core.value","payload":{"value":{"id":7,"total":129.5}}}'
""",
        },
        "",
    ),
    (
        "m-label",
        "label, colour, size, hide",
        "Change how the events you just sent look: a label to find them by, a colour (gray, green, "
        "orange, red, blue, purple), a smaller or larger row, or hide them. glint expect can match "
        "on labels, even ones set afterwards.",
        {
            "php": """
glint($order)->label('before save')->green();
glint($payload)->label('webhook')->small();
glint($secret)->hide();
""",
            "python": """
glint(order).label("before save").green()
glint(payload).label("webhook").small()
glint(secret).hide()
""",
            "node": """
glint(order).label("before save").green();
glint(payload).label("webhook").small();
glint(secret).hide();
""",
            "browser": """
glint(cart).label("cart on load").blue();
""",
            "http": """
# label and colour travel in meta
curl -s localhost:23600/v1/events \\
  -d '{"kind":"core.value","payload":{"value":{"id":7}},"meta":{"label":"before save","color":"green"}}'
""",
        },
        "",
    ),
    (
        "m-log",
        "log(message, fields, level)",
        "A log line with structured fields. Levels are trace, debug, info, warn, error and fatal; "
        "error and fatal count as failures.",
        {
            "php": """
Glint\\Glint::log('payment retried', ['order' => $order->id, 'attempt' => 2], 'warn');
""",
            "python": """
glint.log("payment retried", {"order": order.id, "attempt": 2}, level="warn")
""",
            "node": """
glint.log("payment retried", { order: order.id, attempt: 2 }, "warn");
""",
            "browser": """
glint.log("checkout step", { step: 2, coupon: form.coupon.value });
""",
            "http": """
curl -s localhost:23600/v1/events \\
  -d '{"kind":"core.log","payload":{"message":"payment retried","fields":{"attempt":2}},"meta":{"level":"warn"}}'
""",
        },
        "",
    ),
    (
        "m-table",
        "table(rows, columns)",
        "Rows as a table. Pass the columns to choose and order them; without them the keys of the "
        "first row are used.",
        {
            "php": """
Glint\\Glint::table($order->items->toArray(), ['sku', 'qty', 'price']);
""",
            "python": """
glint.table([{"sku": i.sku, "qty": i.qty, "price": i.price} for i in order.items])
""",
            "node": """
glint.table(order.items, ["sku", "qty", "price"]);
""",
            "browser": """
glint.table(cart.items, ["sku", "qty", "price"]);
""",
            "http": """
curl -s localhost:23600/v1/events \\
  -d '{"kind":"core.table","payload":{"rows":[{"sku":"A-1","qty":2},{"sku":"B-7","qty":1}]}}'
""",
        },
        "",
    ),
    (
        "m-exception",
        "exception(error)",
        "An error with its class, message and stack. The window shows the code around the line it "
        "was thrown from. glint(error) does the same.",
        {
            "php": """
try {
    $gateway->charge($order);
} catch (Throwable $e) {
    glint($e);                       // or Glint\\Glint::exception($e)
    throw $e;
}
""",
            "python": """
try:
    gateway.charge(order)
except Exception as err:
    glint(err)                       # or glint.exception(err)
    raise
""",
            "node": """
try {
  await gateway.charge(order);
} catch (err) {
  glint(err);                        // or glint.exception(err)
  throw err;
}
""",
            "browser": """
window.addEventListener("error", (e) => glint(e.error)); // already done for you; for caught errors:
try { renderCart(cart); } catch (err) { glint(err); }
""",
            "http": """
curl -s localhost:23600/v1/events -d '{"kind":"core.exception","payload":{
  "class":"TypeError","message":"coupon is null",
  "frames":[{"file":"/srv/shop/cart.js","line":11,"fn":"total"}]}}'
""",
        },
        "",
    ),
    (
        "m-measure",
        "measure(name, fn): time it, nest what it sends",
        "Runs fn, sends how long it took, and returns what fn returns. Everything sent while fn runs "
        "nests under the measure, inner measures too, so a run reads as a call tree. In Node and Python "
        "the nesting follows await; in the browser only the synchronous part of fn nests.",
        {
            "php": """
$total = Glint\\Glint::measure('pricing', function () use ($cart) {
    $rates = Glint\\Glint::measure('load rates', fn () => Rate::forCart($cart)); // nests
    return $cart->total($rates);
});
""",
            "python": """
def price():
    rates = glint.measure("load rates", lambda: Rate.for_cart(cart))  # nests
    return cart.total(rates)

total = glint.measure("pricing", price)
""",
            "node": """
const total = await glint.measure("pricing", async () => {
  const rates = await glint.measure("load rates", () => loadRates(cart)); // nests
  return cart.total(rates);
});
""",
            "browser": """
const html = glint.measure("render cart", () => renderCart(cart));
""",
            "http": """
# a measure is an event with a duration; children name it as parent
curl -s localhost:23600/v1/events -d '[
  {"id":"m1","kind":"core.measure","payload":{"name":"pricing","duration_ms":38.9}},
  {"kind":"sql.query","parent":"m1","payload":{"sql":"select * from rates","duration_ms":3.4}}]'
""",
        },
        "",
    ),
    (
        "m-pause",
        "pause(reason): stop until you answer",
        "Stops the program at this line until someone presses Continue in the window (or C), or a "
        "coding agent answers through MCP. It returns the value given with Continue, for example "
        '{"approve": true}, or nothing. Stop raises GlintStop. pause returns at once when Glint is '
        "off or unreachable, and on CI unless GLINT_FORCE_PAUSE is set; after 5 minutes it gives up "
        "and carries on.",
        {
            "php": """
$answer = Glint\\Glint::pause("refund {$order->total} EUR for order {$order->id}?");
if ($answer['approve'] ?? false) {
    $gateway->refund($order);
}
""",
            "python": """
answer = glint.pause(f"refund {order.total} EUR for order {order.id}?")
if answer and answer.get("approve"):
    gateway.refund(order)
""",
            "node": """
const answer = await glint.pause(`refund ${order.total} EUR for order ${order.id}?`);
if (answer?.approve) await gateway.refund(order);
""",
            "browser": """
const answer = await glint.pause("check the discount before charging");
if (answer?.discount) cart.discount = answer.discount;
""",
            "http": """
# 1. send the pause with an id you choose
curl -s localhost:23600/v1/events -d '{"id":"p-42","kind":"ctl.pause","payload":{"reason":"refund?"}}'
# 2. wait for the answer: 204 = still waiting (ask again), 200 = {"action":"continue","value":...}
curl -s "localhost:23600/v1/ctl/p-42?timeout_s=25"
""",
        },
        "",
    ),
    (
        "m-send",
        "send(kind, payload, meta): anything else",
        "Sends an event of any kind as it is. Use it for SQL queries from your own query logger, "
        "HTTP calls, jobs, or kinds of your own (prefix them with x-).",
        {
            "php": """
DB::listen(fn ($q) => Glint\\Glint::send('sql.query', [
    'sql' => $q->sql, 'bindings' => $q->bindings, 'duration_ms' => $q->time, 'conn' => $q->connectionName,
]));
""",
            "python": """
glint.send("sql.query", {"sql": sql, "bindings": params, "duration_ms": 3.4, "conn": "postgres"})
""",
            "node": """
glint.send("sql.query", { sql, bindings: params, duration_ms: 3.4, conn: "mysql" });
""",
            "browser": """
glint.send("x-shop.step", { step: "address", valid: form.checkValidity() });
""",
            "http": """
curl -s localhost:23600/v1/events \\
  -d '{"kind":"sql.query","payload":{"sql":"select * from orders where id = ?","bindings":[7],"duration_ms":3.4}}'
""",
        },
        "",
    ),
    (
        "m-flush",
        "flush(): make sure it was sent",
        "SDKs send in the background without slowing your program. A short script that exits right "
        "away is flushed for you on exit; call flush yourself before a hard exit or before checking "
        "results with glint expect.",
        {
            "php": "Glint\\Glint::flush();",
            "python": "glint.flush()",
            "node": "await glint.flush();",
            "browser": "glint.flush(); // also done when the page is hidden or closed",
            "http": "# each POST is stored before it returns",
        },
        "",
    ),
    (
        "m-configure",
        "configure: address, on/off, project",
        "Usually you need none of this: the defaults find glintd, and the project is the git "
        "repository's name. Environment variables do the same for every language: GLINT_URL, "
        "GLINT_ENABLED, GLINT_PROJECT.",
        {
            "php": "Glint\\Glint::configure(url: 'http://127.0.0.1:23600', enabled: true, project: 'shop');",
            "python": 'glint.configure(url="http://127.0.0.1:23600", enabled=True, project="shop")',
            "node": 'glint.configure({ url: "http://127.0.0.1:23600", enabled: true, project: "shop" });',
            "browser": '<script src="http://127.0.0.1:23600/v1/browser.js" data-project="shop"></script>',
            "http": 'curl -s localhost:23600/v1/events -d \'{"project":"shop","kind":"core.value","payload":{"value":1}}\'',
        },
        "",
    ),
]


def method_card(mid, title, desc, examples):
    tabs = "".join(f'<button role="tab" data-tab="{k}">{n}</button>' for k, n in LANGS)
    panes = "".join(
        f'<div class="pane" data-pane="{k}"{"" if i == 0 else " hidden"}>{code(examples[k], "html" if (k == "browser" and examples[k].lstrip().startswith("<")) else CODE_LANG[k])}</div>'
        for i, (k, _) in enumerate(LANGS)
    )
    return f'<div class="method" id="{mid}"><h3><code>{html.escape(title.split(":")[0])}</code>{html.escape(title[len(title.split(":")[0]):].lstrip(":"))}</h3><p>{html.escape(desc)}</p><div class="tabbed"><div class="tabs" role="tablist">{tabs}</div>{panes}</div></div>'


# ---------------------------------------------------------------- sections
def sections():
    s = []
    s.append(("d-start", "Start", f"""
<h1>Glint shows what your programs did</h1>
<p class="lead">Values, logs, SQL, exceptions, timings and LLM calls from PHP, Python, Node and the browser arrive here as they happen, grouped by project and run. Your coding agent reads the same events.</p>
<h2>Install once</h2>
<p>Open the Glint app and click <b>Set up this Mac</b> (on the first screen, or in Settings under This Mac). The same from a terminal:</p>
{cmd("/Applications/Glint.app/Contents/Resources/bin/glint install")}
<p>This starts <code>glintd</code> at login, makes <code>glint()</code> a global in every PHP, Python and Node program, reports their uncaught errors, and tells the coding agents it finds (Claude Code, Cursor, VS Code, Windsurf, Gemini CLI, Codex) about Glint. Open a new terminal, then check that everything answers:</p>
{cmd("glint doctor")}
<p>Building from source instead:</p>
{cmd("cargo build --release -p glintd -p glint-cli && target/release/glint install")}
<p class="aside">SDKs send nothing in production: when <code>APP_ENV</code> or <code>NODE_ENV</code> is <code>production</code>, or <code>GLINT_ENABLED=0</code>. Events never leave this machine.</p>
"""))
    s.append(("d-api", "Methods", """
<h2>Methods</h2>
<p>After <code>glint install</code>, <code>glint()</code> works in PHP, Python and Node without an import. In PHP the other methods live on <code>Glint\\Glint</code>; in Python, Node and the browser they hang off <code>glint</code>. Pick a language once and every example below follows it.</p>
""" + "".join(method_card(*m[:4]) for m in METHODS) + """
<p class="aside">A project that vendors an SDK itself (from <code>sdks/php</code>, <code>sdks/python</code> or <code>sdks/js</code> in the Glint repository) uses its own copy instead of the global one.</p>
"""))
    s.append(("d-nocode", "With no code", f"""
<h2>What Glint records with no code at all</h2>
<p>Uncaught errors. PHP still prints its error and exits with the same code, Python still prints the traceback, and a Node process still crashes as before. Glint only records them.</p>
<table class="ref">
<tr><td>PHP</td><td>uncaught exceptions and fatal errors (out of memory, a parse error in an included file). Works for scripts, php-fpm, Valet, Herd and <code>php -S -t public</code>. PHP skips the prepend file for a <code>php -S</code> router script; for <code>artisan serve</code> Glint loads it anyway.</td></tr>
<tr><td>Python</td><td>uncaught exceptions in any thread.</td></tr>
<tr><td>Node</td><td>uncaught exceptions and unhandled promise rejections.</td></tr>
<tr><td>Browser</td><td>with one tag in the page you are developing: uncaught errors, unhandled rejections, <code>console.error</code> and <code>console.warn</code>, failed requests and resources that failed to load.</td></tr>
</table>
{cmd('<script src="http://127.0.0.1:23600/v1/browser.js" data-project="shop"></script>', "html")}
<p>Only pages on <code>localhost</code>, <code>127.0.0.1</code>, <code>*.localhost</code> or <code>*.test</code> can send. Leave the tag out of production builds.</p>
<p class="aside">Frameworks catch exceptions to render an error page, so those are never uncaught. Glint hooks into WordPress, Laravel, Django, Flask, FastAPI, Express and Next.js to report them anyway (see each one's page). For another framework, call <code>glint($e)</code> in its error handler.</p>
"""))
    s.append(("d-wp", "WordPress", f"""
<h2>WordPress</h2>
<p>A WordPress site on this machine needs no plugin. Glint recognises a WordPress request and hooks in before WordPress loads, so every page, admin screen, REST call and cron run reports on its own:</p>
<table class="ref">
<tr><td>the request</td><td>method and path first, and at the end a summary: response code, time, number of queries, memory, the template file, the conditional tags that were true, and the main query</td></tr>
<tr><td>SQL</td><td>every <code>$wpdb</code> query with the theme or plugin line that caused it and the rows it returned; database errors count as failures. Timings appear when <code>SAVEQUERIES</code> is on in <code>wp-config.php</code>.</td></tr>
<tr><td>errors</td><td>each <code>WP_Error</code> as it is created, <code>_doing_it_wrong()</code>, deprecation notices from themes and plugins, and PHP warnings</td></tr>
<tr><td>mail</td><td>what <code>wp_mail()</code> sends (to, subject, body) and mail that failed, with the reason</td></tr>
<tr><td>HTTP</td><td>calls made with <code>wp_remote_get()</code> and friends; failed ones count as failures</td></tr>
<tr><td>REST</td><td>responses with an error status, with the route and WordPress's error code. The block editor saves over REST, so "Updating failed" arrives here with the reason.</td></tr>
<tr><td>browser</td><td>the browser script on the front end, in wp-admin and on the login page, so JavaScript errors, failed requests and console errors arrive too</td></tr>
<tr><td>block editor</td><td>invalid blocks, with the saved HTML and what the editor expected, and the editor's error notices</td></tr>
</table>
<p>The editor heartbeat that runs every 15 to 60 seconds is left out. A site whose <code>WP_ENVIRONMENT_TYPE</code> is <code>production</code> is left alone. To keep only some of this, set <code>GLINT_WP</code> (a constant in <code>wp-config.php</code> or an environment variable) to a list such as <code>requests,errors,mail,rest,editor</code>; add <code>hooks</code> to count every action and filter that fired; <code>off</code> turns it all off. WP-CLI commands are reported when <code>GLINT_WP_CLI</code> is set.</p>
<h3>Methods for your code</h3>
{code("""use Glint\\WordPress;

WordPress::post($post_id);          // the post with its fields, meta, terms and permalink
WordPress::user();                  // the current user: roles, capabilities that are on, meta
WordPress::query();                 // the main WP_Query: vars, the SQL it ran, the posts it found
WordPress::query($custom_query);    // or any WP_Query
WordPress::hooks('save_post');      // what listens to a hook: priority, callback, file and line
WordPress::template();              // the template file in use and the theme
WordPress::conditionals();          // is_single, is_admin and the other tags that are true now
WordPress::options('siteurl', 'permalink_structure');
WordPress::queries();               // every query of this request as a table (needs SAVEQUERIES)""", "php")}
<p>Plain <code>glint()</code>, <code>Glint\\Glint::measure()</code> and <code>Glint\\Glint::pause()</code> work in themes and plugins as everywhere else. A pause in a template stops the page until you continue it here.</p>
"""))
    s.append(("d-docker", "Docker and remote", f"""
<h2>Docker and remote machines</h2>
<p>Code in a container or on another machine has no <code>glint install</code>, and 127.0.0.1 there is not this Mac. Run <code>glint docker</code> in the project folder. It writes a copy of the SDKs to <code>~/.glint/sdk/container</code> and prints the lines to add to the service in <code>docker-compose.yml</code>:</p>
{cmd("glint docker                 # compose; --run for docker run, --devcontainer, --ssh HOST")}
{code("""    volumes:
      - /Users/me/.glint/sdk/container:/opt/.glint/sdk:ro
    environment:
      GLINT_URL: http://host.docker.internal:23600
      GLINT_PROJECT: shop
      GLINT_PATH_MAP: /var/www/html:/Users/me/shop
      PHP_INI_SCAN_DIR: ":/opt/.glint/sdk/php/conf.d"
    extra_hosts:
      - host.docker.internal:host-gateway""", "yaml")}
<p>Recreate the container and <code>glint()</code> works in PHP, Python and Node there, and uncaught errors arrive as they do on the Mac. <code>PHP_INI_SCAN_DIR</code> loads the prepend file, <code>PYTHONPATH</code> runs a <code>sitecustomize.py</code>, and <code>NODE_OPTIONS</code> imports the global; the command sets only the ones for the languages it finds in the folder (<code>--lang</code> to choose).</p>
<table class="ref">
<tr><td><code>GLINT_URL</code></td><td>where glintd is. Inside a container the SDKs already try <code>http://host.docker.internal:23600</code> when it is not set.</td></tr>
<tr><td><code>GLINT_PROJECT</code></td><td>the project. A path like <code>/var/www/html</code> does not say which repository it is, so the command names the git repository of this folder.</td></tr>
<tr><td><code>GLINT_PATH_MAP</code></td><td><code>container path:Mac path</code>, several separated by commas. glintd rewrites file paths and stack frames with it, so the source view and Open in editor show the files on this Mac. Use <code>--app-dir</code> when your compose file mounts the code somewhere else.</td></tr>
</table>
<h3>Docker on Linux</h3>
<p>Docker Desktop on a Mac passes <code>host.docker.internal</code> to the Mac's 127.0.0.1, so nothing else is needed there. On Linux, <code>host-gateway</code> is the Docker bridge, which glintd does not listen on. Set <code>remote_addr</code> in Settings, or in <code>~/.glint/config.toml</code>, to the bridge address and restart glintd:</p>
{code('remote_addr = "172.17.0.1:23600"', "toml")}
<p>That address only takes events: a program there can send and wait for its own pause, and cannot read events, settings or files. glintd retries every 10 seconds when Docker creates the bridge after it started. Do not use <code>0.0.0.0</code>: every network this machine is on could then write events.</p>
<h3>Dev containers</h3>
<p><code>glint docker --devcontainer</code> prints <code>mounts</code>, <code>runArgs</code> and <code>containerEnv</code> for <code>.devcontainer/devcontainer.json</code>. The path map there is <code>${{containerWorkspaceFolder}}:${{localWorkspaceFolder}}</code>, so it fits any checkout.</p>
<h3>SSH</h3>
<p>Keep glintd on the Mac and open a reverse tunnel, so 127.0.0.1:23600 on the remote machine reaches it:</p>
{cmd("ssh -R 23600:127.0.0.1:23600 devbox")}
<p>Or put <code>RemoteForward 23600 127.0.0.1:23600</code> under the host in <code>~/.ssh/config</code>; VS Code Remote SSH reads that file too. <code>glint docker --ssh devbox</code> prints the commands that copy the SDK there and the lines for the remote shell profile. Set <code>GLINT_PATH_MAP</code> only when the same code is also on the Mac; otherwise the files open only on the remote machine.</p>
"""))
    s.append(("d-laravel", "Laravel", f"""
<h2>Laravel</h2>
<p>A Laravel app needs no package and no code. When <code>public/index.php</code>, <code>artisan serve</code>, an artisan command or a queue worker runs inside a Laravel app, Glint hooks in as the app boots and reports:</p>
<table class="ref">
<tr><td>the request</td><td>one run per request with a summary at its root: method, route, controller action, views, response code, time, memory and number of queries. Everything else nests under it, so the call tree shows the request.</td></tr>
<tr><td>SQL</td><td>every query with its bindings, time, connection and the line in your app that ran it. A query from a Blade view points at the template, not the compiled file.</td></tr>
<tr><td>exceptions</td><td>what the exception handler reports, with the request: URL, route, action and input (passwords, tokens and keys hidden)</td></tr>
<tr><td>logs</td><td>everything written with <code>Log::</code>, at its level, with the line that wrote it</td></tr>
<tr><td>queue jobs</td><td>in a worker each job is a run of its own, with its SQL and exception under it; a failed job counts as a failure. A job on the sync driver nests under the request that dispatched it.</td></tr>
<tr><td>HTTP calls</td><td>calls of the HTTP client (<code>Http::get()</code> and friends) that failed: connection errors, 4xx and 5xx</td></tr>
</table>
<p>How it gets in: Laravel defines its <code>env()</code> helper only when none exists and calls it while the configuration loads. Glint defines an identical <code>env()</code> first and hooks in on that first call. With a cached configuration (<code>php artisan config:cache</code>) the call never happens. Clear the cache in development, or require the SDK with Composer, and Laravel discovers its service provider:</p>
{cmd("composer require --dev glint/glint")}
<p>An app with <code>APP_ENV=production</code> is left alone. <code>GLINT_LARAVEL=off</code> in the environment turns the integration off.</p>
"""))
    s.append(("d-django", "Django", f"""
<h2>Django</h2>
<p>A Django project needs no middleware and no settings. Glint hooks in when Django's request handler loads, under <code>runserver</code>, gunicorn, uvicorn or any other server, sync or async:</p>
<table class="ref">
<tr><td>the request</td><td>one run per request with a summary at its root: method, route, view, response code, time, memory and number of queries</td></tr>
<tr><td>SQL</td><td>every query of every database connection with its parameters, time, rows and the line in your code that ran it; a failed query counts as a failure</td></tr>
<tr><td>exceptions</td><td>what Django turns into an error page, with the request it happened in</td></tr>
<tr><td>logs</td><td>records of your own loggers, and warnings and errors of Django's, with the line that wrote them</td></tr>
<tr><td>HTTP calls</td><td>calls made with <code>requests</code> or <code>httpx</code> that failed: connection errors, 4xx and 5xx</td></tr>
</table>
<p>It works through the Python shim from <code>glint install</code>, so a virtual environment needs nothing either. <code>GLINT_FRAMEWORKS=off</code> turns the framework hooks off.</p>
"""))
    s.append(("d-flask", "Flask and FastAPI", f"""
<h2>Flask and FastAPI</h2>
<p>Flask, FastAPI and other Starlette apps report on their own, like Django:</p>
<table class="ref">
<tr><td>the request</td><td>one run per request with a summary at its root: method, route, endpoint, response code, time, memory and number of queries</td></tr>
<tr><td>SQL</td><td>every statement that goes through SQLAlchemy (also Flask-SQLAlchemy and SQLModel), with its parameters, time, rows and the line in your code that ran it</td></tr>
<tr><td>exceptions</td><td>unhandled exceptions, with the request they happened in</td></tr>
<tr><td>logs</td><td>records of your own loggers, with the line that wrote them</td></tr>
<tr><td>HTTP calls</td><td>calls made with <code>requests</code> or <code>httpx</code> that failed</td></tr>
</table>
<p>Async endpoints keep their request: what an awaited function sends still nests under it.</p>
"""))
    s.append(("d-express", "Express", f"""
<h2>Express and other Node servers</h2>
<p>Every HTTP server in a Node process reports its requests: Express, Fastify, Koa or plain <code>node:http</code>. For Express, Glint also knows the route and catches errors:</p>
<table class="ref">
<tr><td>the request</td><td>one run per request with a summary at its root: method, route pattern, response code, time, memory and number of queries</td></tr>
<tr><td>SQL</td><td>queries of <code>pg</code>, <code>mysql2</code> and <code>node:sqlite</code> with their values, time, rows and the line in your code that ran them, also through a pool</td></tr>
<tr><td>exceptions</td><td>errors that reach Express's error handling (thrown in a route or passed to <code>next(err)</code>), with the request. Glint looks before your own error handler, which still runs.</td></tr>
<tr><td>HTTP calls</td><td><code>fetch()</code> calls that failed: network errors, 4xx and 5xx</td></tr>
</table>
<p>This comes with the Node shim from <code>glint install</code> (<code>NODE_OPTIONS</code>). Prisma is not hooked; send its queries yourself if you need them:</p>
{code("""const prisma = new PrismaClient({ log: [{ emit: "event", level: "query" }] });
prisma.$on("query", (e) => glint.send("sql.query", { sql: e.query, bindings: JSON.parse(e.params), duration_ms: e.duration }));""", "js")}
<p><code>GLINT_FRAMEWORKS=off</code> turns these hooks off; <code>NODE_ENV=production</code> turns Glint off.</p>
"""))
    s.append(("d-next", "Next.js", f"""
<h2>Next.js</h2>
<p><code>next dev</code> reports each page and route handler request as a run, with the SQL of <code>pg</code>, <code>mysql2</code> and <code>node:sqlite</code> under it, failed <code>fetch()</code> calls, and the errors Next.js prints for a route, with the request. Requests for <code>/_next/</code> assets are left out.</p>
<p>For the browser side, add the browser script to the root layout in development:</p>
{code("""// app/layout.js
export default function RootLayout({ children }) {
  return (
    <html lang="en">
      <head>
        {process.env.NODE_ENV === "development" && (
          <script src="http://127.0.0.1:23600/v1/browser.js" data-project="shop" />
        )}
      </head>
      <body>{children}</body>
    </html>
  );
}""", "js")}
"""))
    s.append(("d-vite", "Vite", f"""
<h2>Vite</h2>
<p>The Vite plugin puts the browser script on the dev server's pages with the right project name, so the browser's errors, failed requests and <code>window.glint()</code> markers arrive here. A production build stays as it is.</p>
{code("""// vite.config.js
import glint from "glint-sdk/vite"; // sdks/js in the Glint repository, or ~/.glint/sdk/js

export default { plugins: [glint()] };""", "js")}
<p>Options: <code>glint({{ project: "shop", url: "http://127.0.0.1:23600" }})</code>. Without them the project is the git repository's name and the address comes from <code>GLINT_URL</code>.</p>
"""))
    s.append(("d-tree", "Call tree", f"""
<h2>Call tree and timing</h2>
<p>Wrap work in <code>measure()</code> and everything it sends nests under it. A checkout request then reads <code>checkout → load order → select …</code> instead of a flat list. Click the arrow left of a row to fold it. The bars on the right are a small waterfall: where in the run each piece started and how long it took. OpenTelemetry spans nest the same way.</p>
{code('''Glint\\Glint::measure('checkout', function () use ($id) {
    $order = Glint\\Glint::measure('load order', fn () => Order::find($id)); // its SQL nests here
    glint($order)->label('order');
});''', "php")}
"""))
    s.append(("d-expect", "glint expect", f"""
<h2>Checks with glint expect</h2>
<p>Markers you placed with <code>glint()</code> become a test. <code>glint expect</code> waits until events match each expectation, in order, and fails as soon as a program errors. It prints which event satisfied each line.</p>
{cmd("""glint expect 'label:cart payload.value.total>0' 'kind:sql.query text:"insert into orders"' \\
             'label:"order saved" payload.value.paid=true' -- ./checkout.sh 7""")}
<table class="ref">
<tr><td><code>label:X</code></td><td>the event's label, also one set afterwards with <code>.label()</code></td></tr>
<tr><td><code>kind:sql.query</code>, <code>kind:sql.*</code></td><td>the kind, or a prefix</td></tr>
<tr><td><code>text:refund</code></td><td>the payload contains it, in any case</td></tr>
<tr><td><code>payload.value.total&gt;0</code></td><td>a dot path compared with <code>= != &gt; &gt;= &lt; &lt;=</code>; <code>true</code>, <code>null</code> and numbers are JSON</td></tr>
<tr><td><code>has:payload.value.email</code></td><td>present and not null</td></tr>
<tr><td><code>count&gt;=2</code></td><td>how many matching events are needed</td></tr>
</table>
<p>It exits with 0 when everything matched, 1 when something did not, and 2 when it could not check. Without <code>-- command</code> it also looks at the last 30 seconds and waits while you click through the app (<code>--timeout</code>). Other flags: <code>--unordered</code>, <code>--allow-errors</code>, <code>-p project</code>. Agents run the same check through the MCP tool <code>glint_expect</code>.</p>
"""))
    s.append(("d-agents", "Coding agents", f"""
<h2>Coding agents</h2>
<p><code>glint install</code> registers Glint's MCP server with the agents it finds. Claude Code also gets hooks, so its session shows up here next to your program and Glint can tell it things:</p>
<ul>
<li>At session start it learns that Glint records this project, that <code>glint()</code> needs no import, and which failures happened in the last day. When glintd is not running it still learns all that, plus that Glint is off and how to start it.</li>
<li>With each of your prompts it gets the failures that are new since the previous one. When you write "it's broken", it already has <code>TypeError … cart.js:11</code>.</li>
<li>When it wants to finish a turn during which your programs failed, it is sent back once to fix them or explain why they are expected. A failure in a file it edited afterwards is left out once a later run of the same program passed; until then it is listed as possibly fixed.</li>
</ul>
<p>Both can be turned off in Settings.</p>
<p>The tools read the project the agent works in, and every answer starts with the project it covers. <code>glint mcp</code> (Codex) sends its folder's project; Claude Code, VS Code and Cursor report their workspace folder when Glint asks. For another client, add <code>?project=shop</code> to the server's URL or pass <code>project</code> to a tool. When Glint cannot tell and several projects have events, the answer lists them instead of mixing them; <code>project="*"</code> reads all.</p>
<table class="ref">
<tr><td><code>glint_last_error</code></td><td>the newest failure with its stack and values</td></tr>
<tr><td><code>glint_events</code>, <code>glint_event</code>, <code>glint_runs</code></td><td>filter events, read one in full, list recent runs</td></tr>
<tr><td><code>glint_wait_for</code></td><td>the matching event of the last 10 seconds, or the next one to arrive after an edit</td></tr>
<tr><td><code>glint_expect</code></td><td>check that markers arrived with the right values and nothing failed</td></tr>
<tr><td><code>glint_pauses</code>, <code>glint_resume</code></td><td>answer a program stopped at <code>pause()</code></td></tr>
<tr><td><code>glint_annotate</code>, <code>glint_show</code></td><td>leave a note on an event; show you markdown or a diagram</td></tr>
<tr><td><code>glint_llm_calls</code>, <code>glint_clear</code></td><td>LLM calls with tokens and cost; start a clean view</td></tr>
</table>
<h3>Does the agent use it?</h3>
<p>Glint keeps a log of what agents do with it: each MCP tool call (which agent, which tool, how long, how big the answer, whether it showed a failure) and each hook action (context given, stop guard fired, then fixed or explained). The log stays in Glint's database on this machine and is deleted with the events. Press <kbd>G</kbd> for the Agents view: per day and project, which agents connected, their calls by tool, how many failures an agent read and how soon. In a run, a failure an agent read says <code>read by Claude Code 14:02</code>; other rows say it when you open them.</p>
{cmd("glint stats --days 7")}
<p>prints the same numbers. <code>examples/agent-eval</code> has a small experiment that has Claude Code fix seeded bugs with and without Glint and compares turns, time and success.</p>
<p>To set it up by hand:</p>
{cmd("claude mcp add --scope user --transport http glint http://127.0.0.1:23600/mcp")}
{cmd("glint hook --print-config")}
"""))
    s.append(("d-cli", "Command line", """
<h2>Command line</h2>
<table class="ref">
<tr><td><code>glint send 42 '{"a":1}' hello</code></td><td>send values; <code>--kind core.log --level warn</code>, <code>--label</code></td></tr>
<tr><td><code>npm run dev 2&gt;&amp;1 | glint pipe -l web</code></td><td>forward a program's output line by line; levels and stack traces are recognised</td></tr>
<tr><td><code>glint expect … -- command</code></td><td>a check from markers</td></tr>
<tr><td><code>glint install</code></td><td>set up this machine; <code>--only service,php,python,node,agents</code>, <code>--dry-run</code></td></tr>
<tr><td><code>glint doctor</code></td><td>is glintd up, does <code>glint()</code> work in each language, which agents know Glint, how big the data is</td></tr>
<tr><td><code>glint uninstall</code></td><td>undo install; your data in <code>~/.glint</code> stays</td></tr>
<tr><td><code>glint stats</code></td><td>what coding agents did with Glint: agents, tool calls, failures read and how soon; <code>--days</code>, <code>-p project</code>, <code>--json</code></td></tr>
<tr><td><code>glint mcp</code>, <code>glint hook</code></td><td>MCP over stdio; the command agent hooks run</td></tr>
<tr><td><code>glint conformance -- node my-sdk.mjs</code></td><td>check an SDK for a new language against the spec</td></tr>
</table>
"""))
    s.append(("d-app", "This window", """
<h2>Using this window</h2>
<p>The top line covers every project: it says what failed and where, which program waits for you, or that all is quiet everywhere. Below it is one run at a time, the newest by when its last event happened. Switch runs and projects with the menus at the top or with <kbd>⌘K</kbd>; a project with a failure or a pause opens on that run.</p>
<table class="ref">
<tr><td><kbd>⌘K</kbd></td><td>jump to a run, project or action; it also finds what events say, in every project, and opens a match in its run</td></tr>
<tr><td><kbd>E</kbd>, <kbd>C</kbd></td><td>show the failure; continue a paused program</td></tr>
<tr><td><kbd>G</kbd></td><td>what coding agents did with Glint</td></tr>
<tr><td><kbd>/</kbd></td><td>search this run, this project or every project (the menu in the search field): words, <code>kind:sql</code>, <code>level:warn</code></td></tr>
<tr><td><kbd>j</kbd> <kbd>k</kbd>, <kbd>[</kbd> <kbd>]</kbd></td><td>move between events (the arrow keys do too), between runs</td></tr>
<tr><td><kbd>o</kbd>, <kbd>⌘C</kbd></td><td>open the focused event's file in your editor; copy the event as text</td></tr>
<tr><td><kbd>←</kbd> <kbd>→</kbd></td><td>fold or unfold what happened inside the focused event</td></tr>
<tr><td><kbd>?</kbd>, <kbd>,</kbd></td><td>these docs, Settings</td></tr>
</table>
<p>Click an event to open it: values as foldable JSON, the code around a failing line, the stack. Click a <code>file:line</code> to open it in your editor. In the desktop app, <kbd>⌥⌘G</kbd> shows and hides the window from anywhere, and the menu-bar spark turns red on a failure (its menu names the run and counts today's failures), amber when a program waits, and is struck through when glintd is not running. If glintd stops, this window says so, marks what it shows as out of date, and picks up again when glintd is back.</p>
"""))
    s.append(("d-config", "Settings and data", """
<h2>Settings and data</h2>
<p>Open Settings with <kbd>,</kbd> or the gear at the top. There you choose how long events are kept and how many, which editor files open in, and whether coding agents hear about failures. Changes apply right away and are saved to <code>~/.glint/config.toml</code>, which you can also edit by hand. An environment variable with the same name (<code>GLINT_KEEP_DAYS</code>, <code>GLINT_EDITOR</code>, …) wins over both, and Settings shows which ones are set that way.</p>
<p>By default Glint keeps 7 days and at most 100,000 events. Every 10 minutes it deletes older ones and the database file shrinks with them. The database also stays under 500 MB: glintd checks every 10 seconds and deletes the oldest events when it is crossed. <span id="docsData"></span></p>
<p>Everything lives in <code>~/.glint</code>: the database, the settings, the SDK copies and <code>glintd.log</code>.</p>
"""))
    s.append(("d-secrets", "Secrets", """
<h2>Secrets</h2>
<p>glintd replaces passwords, tokens and keys in every event before it stores it, so they never reach the database, this window or a coding agent, which sends what it reads to a cloud model. It works the same for every SDK, OpenTelemetry and <code>glint pipe</code>. An event that had secrets says how many were replaced when you open it.</p>
<p>Three things are caught:</p>
<ul>
<li>Values under keys that look secret, at any depth: names containing <code>password</code>, <code>passwd</code>, <code>secret</code>, <code>api_key</code>, <code>authorization</code>, <code>cookie</code>, <code>private_key</code>, <code>access_key</code>, <code>credentials</code>, and names ending in <code>token</code> or <code>dsn</code>. Case, <code>_</code>, <code>-</code> and dots are ignored, so <code>X-Api-Key</code> and <code>apiKey</code> count. Header pairs and SQL bindings count too: in <code>WHERE password = ?</code> the bound value is replaced.</li>
<li>Well-known shapes anywhere in text: <code>Bearer</code> tokens, JWTs, AWS <code>AKIA</code> keys, GitHub <code>ghp_</code> and <code>github_pat_</code>, Slack <code>xox</code>, Stripe <code>sk_live_</code>, OpenAI and Anthropic <code>sk-</code>, Google <code>AIza</code>, PEM private keys, the password in <code>postgres://user:pass@host</code>, and <code>password=...</code> in log lines and SQL.</li>
<li>Payment, identity and session data: card numbers (keys such as <code>card_number</code>, <code>cvv</code>, <code>cvc</code>, and any 13 to 19 digit number in text that passes the Luhn check; a bare <code>card</code> or <code>cc</code> key only when it holds a card number), <code>ssn</code>, <code>otp</code> and <code>pin</code> as the last word of a key, <code>session</code>, <code>PHPSESSID</code>, <code>csrf</code> and <code>xsrf</code>, and <code>sig</code> or <code>signature</code> values such as <code>X-Amz-Signature=...</code>, in keys and in <code>key=value</code> text.</li>
</ul>
<p>A replaced value reads <code>[redacted]</code>; a known shape keeps its prefix, as in <code>sk-ant-…[redacted]</code>, so you can still tell which key was used. A URL keeps everything but its password.</p>
<p>Left alone on purpose: counters and sizes such as <code>token_count</code>, <code>max_tokens</code> and <code>password_length</code>, pagination cursors such as <code>next_token</code>, and ids such as <code>session_id</code>, because a debugging session follows them from event to event. Add any of them to the extra keys if you want them hidden.</p>
<p>In Settings, under Secrets, you can add key names, add regular expressions (a group named <code>s</code> hides only that part), list keys that are never hidden, or turn redaction off. The same settings are <code>redact</code>, <code>redact_keys</code>, <code>redact_patterns</code> and <code>redact_allow</code> in <code>~/.glint/config.toml</code>, or <code>GLINT_REDACT</code>, <code>GLINT_REDACT_KEYS</code> (comma separated), <code>GLINT_REDACT_PATTERNS</code> (one per line) and <code>GLINT_REDACT_ALLOW</code> in glintd's environment. Changes apply to events that arrive afterwards; stored events stay as they are.</p>
"""))
    s.append(("d-langs", "Other languages", f"""
<h2>Adding a language</h2>
<p>Any language can send events today with an HTTP POST (the HTTP tab above). A real SDK is small, because glintd does the hard parts: storage, search, projects, worktrees, the UI and MCP. An SDK needs to:</p>
<ol>
<li>Build envelopes (<code>kind</code>, <code>payload</code>, <code>origin</code> with file and line of the caller) and send them in the background, in batches, never blocking or throwing into the program.</li>
<li>Turn the language's values into JSON with types, cycles and depth limits (<code>docs/PROTOCOL.md</code> §3).</li>
<li>Implement the methods above: value, label and the rest, log, table, exception, measure with nesting, pause, send, flush.</li>
<li>Stay off in production and when <code>GLINT_ENABLED=0</code>.</li>
<li>Pass the conformance suite, which checks all of this against a real glintd:</li>
</ol>
{cmd("glint conformance -- ruby sdks/ruby/conformance.rb")}
<p>For the zero-setup global and the uncaught-error capture, each runtime has its own hook. These are the ones Glint would use:</p>
<table class="ref">
<tr><td>Ruby</td><td><code>RUBYOPT=-r…/glint.rb</code> in the shell profile loads it into every process; <code>at_exit</code> with <code>$!</code> sees uncaught exceptions.</td></tr>
<tr><td>Go</td><td>no global code injection: an import (<code>glint.V(x)</code>), with <code>defer glint.Recover()</code> for panics.</td></tr>
<tr><td>Java, Kotlin</td><td><code>JAVA_TOOL_OPTIONS=-javaagent:glint.jar</code> loads an agent into every JVM; <code>Thread.setDefaultUncaughtExceptionHandler</code> sees crashes.</td></tr>
<tr><td>.NET</td><td><code>DOTNET_STARTUP_HOOKS</code> runs code before Main; <code>AppDomain.UnhandledException</code> sees crashes.</td></tr>
<tr><td>Rust</td><td>a crate with a <code>glint!()</code> macro that knows file and line; a panic hook for crashes.</td></tr>
<tr><td>Elixir, Erlang</td><td>a Logger backend and <code>:telemetry</code> handlers, so existing logs and spans arrive with no code.</td></tr>
</table>
<p>OpenTelemetry already covers many of these for traces and LLM calls. <code>spec/conformance/README.md</code> has the full checklist.</p>
"""))
    s.append(("d-help", "When something is off", """
<h2>When something is off</h2>
<table class="ref">
<tr><td><code>glint()</code> is undefined</td><td>Open a new terminal: Node reads it from your shell profile. Code run with <code>php -r</code> never gets it; a script file does. Run <code>glint doctor</code>.</td></tr>
<tr><td>nothing arrives</td><td>Check that glintd answers with <code>glint doctor</code>. Production mode and <code>GLINT_ENABLED=0</code> turn SDKs off.</td></tr>
<tr><td>a port is taken</td><td>An OpenTelemetry collector may own 4318. Glint then starts without its OpenTelemetry port. Change the main address in Settings.</td></tr>
<tr><td>the wrong project</td><td>Projects are named after the git repository. Set <code>GLINT_PROJECT</code>, or <code>data-project</code> on the browser tag.</td></tr>
<tr><td>start over</td><td>Delete all events in Settings, or run <code>glint uninstall</code> and delete <code>~/.glint</code>.</td></tr>
</table>
"""))
    return s


def build():
    secs = sections()
    nav = "".join(f'<a href="#{sid}">{html.escape(label)}</a>' for sid, label, _ in secs)
    body = "".join(f'<section id="{sid}">{content}</section>' for sid, _, content in secs)
    block = f"""<!-- docs:start (generated by ui/docs/build.py; edit there) -->
<div class="docs" id="docs" role="dialog" aria-modal="true" aria-labelledby="docsTitle" hidden>
  <div class="docs-shell">
    <nav class="docs-nav" aria-label="Documentation">
      <div class="docs-brand"><svg class="mark" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 1.5l1.9 8.6 8.6 1.9-8.6 1.9L12 22.5l-1.9-8.6L1.5 12l8.6-1.9z"/></svg><span id="docsTitle">Glint docs</span></div>
      {nav}
      <button class="link docs-close" data-act="docs-close">Close <kbd>esc</kbd></button>
    </nav>
    <article class="docs-body">{body}</article>
  </div>
</div>
<!-- docs:end -->"""
    text = INDEX.read_text()
    if "<!-- docs:start" in text:
        text = re.sub(r"<!-- docs:start.*?<!-- docs:end -->", lambda _: block, text, flags=re.S)
    else:
        start = text.index('<div class="docs" id="docs"')
        end = text.index('<div class="toast" id="toast"')
        text = text[:start] + block + "\n" + text[end:]
    for bad in ("—", "–"):
        assert bad not in block, "no dashes in the docs (see the writing rules at the top)"
    INDEX.write_text(text)
    print(f"docs: {len(secs)} sections, {len(METHODS)} methods x {len(LANGS)} languages")


if __name__ == "__main__":
    build()
