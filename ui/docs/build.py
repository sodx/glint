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
<h3>Install once</h3>
{cmd("cargo build --release -p glintd -p glint-cli && target/release/glint install")}
<p>This starts <code>glintd</code> at login, makes <code>glint()</code> a global in every PHP, Python and Node program, reports their uncaught errors, and tells the coding agents it finds (Claude Code, Cursor, VS Code, Windsurf, Gemini CLI, Codex) about Glint. Open a new terminal, then check that everything answers:</p>
{cmd("glint doctor")}
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
<tr><td>PHP</td><td>uncaught exceptions and fatal errors (out of memory, a parse error in an included file). Works for scripts, php-fpm, Valet, Herd and <code>php -S -t public</code>. PHP skips the prepend file for a <code>php -S</code> router script, which is what <code>artisan serve</code> uses.</td></tr>
<tr><td>Python</td><td>uncaught exceptions in any thread.</td></tr>
<tr><td>Node</td><td>uncaught exceptions and unhandled promise rejections.</td></tr>
<tr><td>Browser</td><td>with one tag in the page you are developing: uncaught errors, unhandled rejections, <code>console.error</code> and <code>console.warn</code>, failed requests and resources that failed to load.</td></tr>
</table>
{cmd('<script src="http://127.0.0.1:23600/v1/browser.js" data-project="shop"></script>', "html")}
<p>Only pages on <code>localhost</code>, <code>127.0.0.1</code>, <code>*.localhost</code> or <code>*.test</code> can send. Leave the tag out of production builds.</p>
<p class="aside">Frameworks that catch exceptions to render an error page (Laravel, Express, Django) stop them from being uncaught. Call <code>glint($e)</code> in the framework's error handler to see them.</p>
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
<li>At session start it learns that Glint records this project, that <code>glint()</code> needs no import, and which failures happened in the last day.</li>
<li>With each of your prompts it gets the failures that are new since the previous one. When you write "it's broken", it already has <code>TypeError … cart.js:11</code>.</li>
<li>When it wants to finish a turn during which your programs failed, it is sent back once to fix them or explain why they are expected. Failures in files it edited afterwards don't count.</li>
</ul>
<p>Both can be turned off in Settings.</p>
<table class="ref">
<tr><td><code>glint_last_error</code></td><td>the newest failure with its stack and values</td></tr>
<tr><td><code>glint_events</code>, <code>glint_event</code>, <code>glint_runs</code></td><td>filter events, read one in full, list recent runs</td></tr>
<tr><td><code>glint_wait_for</code></td><td>wait for the next matching event after an edit</td></tr>
<tr><td><code>glint_expect</code></td><td>check that markers arrived with the right values and nothing failed</td></tr>
<tr><td><code>glint_pauses</code>, <code>glint_resume</code></td><td>answer a program stopped at <code>pause()</code></td></tr>
<tr><td><code>glint_annotate</code>, <code>glint_show</code></td><td>leave a note on an event; show you markdown or a diagram</td></tr>
<tr><td><code>glint_llm_calls</code>, <code>glint_clear</code></td><td>LLM calls with tokens and cost; start a clean view</td></tr>
</table>
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
<tr><td><code>glint mcp</code>, <code>glint hook</code></td><td>MCP over stdio; the command agent hooks run</td></tr>
<tr><td><code>glint conformance -- node my-sdk.mjs</code></td><td>check an SDK for a new language against the spec</td></tr>
</table>
"""))
    s.append(("d-app", "This window", """
<h2>Using this window</h2>
<p>The top line says what failed and where, which program waits for you, or that all is quiet. Below it is one run at a time. Switch runs and projects with the menus at the top or with <kbd>⌘K</kbd>.</p>
<table class="ref">
<tr><td><kbd>⌘K</kbd></td><td>jump to a run, project or action</td></tr>
<tr><td><kbd>E</kbd>, <kbd>C</kbd></td><td>show the failure; continue a paused program</td></tr>
<tr><td><kbd>/</kbd></td><td>search this run: words, <code>kind:sql</code>, <code>level:warn</code></td></tr>
<tr><td><kbd>j</kbd> <kbd>k</kbd>, <kbd>[</kbd> <kbd>]</kbd></td><td>move between events, between runs</td></tr>
<tr><td><kbd>?</kbd>, <kbd>,</kbd></td><td>these docs, Settings</td></tr>
</table>
<p>Click an event to open it: values as foldable JSON, the code around a failing line, the stack. Click a <code>file:line</code> to open it in your editor. In the desktop app, <kbd>⌥⌘G</kbd> shows and hides the window from anywhere, and the menu-bar spark turns red on a failure and amber when a program waits.</p>
"""))
    s.append(("d-config", "Settings and data", """
<h2>Settings and data</h2>
<p>Open Settings with <kbd>,</kbd> or the gear at the top. There you choose how long events are kept and how many, which editor files open in, and whether coding agents hear about failures. Changes apply right away and are saved to <code>~/.glint/config.toml</code>, which you can also edit by hand. An environment variable with the same name (<code>GLINT_KEEP_DAYS</code>, <code>GLINT_EDITOR</code>, …) wins over both, and Settings shows which ones are set that way.</p>
<p>By default Glint keeps 7 days and at most 100,000 events. Every 10 minutes it deletes older ones and the database file shrinks with them, so it stays at a few hundred MB at most. <span id="docsData"></span></p>
<p>Everything lives in <code>~/.glint</code>: the database, the settings, the SDK copies and <code>glintd.log</code>.</p>
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
<p>Ray's client libraries already work for many of these through Glint's Ray-compatible port, and OpenTelemetry covers the rest for traces and LLM calls. <code>spec/conformance/README.md</code> has the full checklist.</p>
"""))
    s.append(("d-help", "When something is off", """
<h2>When something is off</h2>
<table class="ref">
<tr><td><code>glint()</code> is undefined</td><td>Open a new terminal: Node reads it from your shell profile. Code run with <code>php -r</code> never gets it; a script file does. Run <code>glint doctor</code>.</td></tr>
<tr><td>nothing arrives</td><td>Check that glintd answers with <code>glint doctor</code>. Production mode and <code>GLINT_ENABLED=0</code> turn SDKs off.</td></tr>
<tr><td>a port is taken</td><td>Ray's app owns 23517 and an OpenTelemetry collector 4318. Glint starts without those. Change the main address in Settings.</td></tr>
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
