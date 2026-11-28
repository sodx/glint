<?php

declare(strict_types=1);

namespace Glint;

use Throwable;

/**
 * WordPress, with no plugin and no code: `glint install` prepends Glint to every PHP request, and
 * when the request is a WordPress one, this class registers its hooks before WordPress loads
 * (through the `$wp_filter` array WordPress reads at start).
 *
 * What it reports on its own:
 * - the request (method and path), and at the end a summary: template, conditionals, main query,
 *   number of queries, memory, time, response code;
 * - every SQL query of `$wpdb` with its caller, rows, and database errors as failures (timings too
 *   when SAVEQUERIES is on);
 * - `WP_Error`s as they are created, `_doing_it_wrong()` and deprecation notices, PHP warnings;
 * - mail sent with `wp_mail()` and mail that failed;
 * - HTTP API requests (`wp_remote_get()` and friends), failed ones as errors;
 * - REST API responses with an error status (the block editor talks to WordPress over REST, so
 *   "Updating failed" shows up here with the reason);
 * - in the browser (front end, admin, login): the browser script, so JavaScript errors arrive too,
 *   and in the block editor: invalid blocks and the editor's error notices.
 *
 * Opt out per feature with GLINT_WP (environment variable or constant), a comma list of what to
 * keep: `requests,queries,errors,mail,http,rest,notices,browser,editor`, plus `hooks` (off by
 * default: counts every action and filter). `GLINT_WP=off` turns the integration off.
 *
 * Only development sites report: WP_ENVIRONMENT_TYPE `local` or `development`, or WP_DEBUG on, or
 * GLINT_ENABLED=1. Any other site (WordPress treats a site without WP_ENVIRONMENT_TYPE as
 * production) is left alone: nothing is sent, glint() does nothing and no browser script is added.
 * Until WordPress has loaded wp-config.php and that is known, events are queued, not sent.
 *
 * Methods for code: WordPress::post(), user(), query(), hooks(), template(), conditionals(),
 * options(), queries().
 */
final class WordPress
{
    private const DEFAULT = ['requests', 'queries', 'errors', 'mail', 'http', 'rest', 'notices', 'browser', 'editor'];

    /** @var list<string> */
    private static array $features = [];
    private static float $t0 = 0.0;
    /** @var array{sql: string, site: array<string, mixed>}|null the query sent to the database last */
    private static ?array $pending = null;
    private static int $queries = 0;
    private static ?string $template = null;
    /** @var array<string, int> */
    private static array $hookCounts = [];
    private static bool $off = false;
    /** @var array<string, true> WP_Errors already reported in this request */
    private static array $seenErrors = [];

    // ------------------------------------------------------------------ start

    /**
     * Called by the prepend file for every request: when it looks like WordPress, registers the
     * hooks WordPress will pick up when it loads. Does nothing otherwise.
     */
    public static function preload(): void
    {
        try {
            if (!self::looksLikeWordPress()) {
                return;
            }
            $want = self::setting('GLINT_WP');
            if ($want !== null && in_array(strtolower($want), ['0', 'off', 'false', 'no'], true)) {
                return;
            }
            if (($_POST['action'] ?? $_GET['action'] ?? '') === 'heartbeat') {
                return; // the editor's keep-alive every 15-60 s: noise
            }
            self::$features = $want === null || $want === '' ? self::DEFAULT : array_map('trim', explode(',', strtolower($want)));
            self::$t0 = microtime(true);

            $hooks = [
                ['muplugins_loaded', 0, 'boot', 0],
                ['shutdown', PHP_INT_MAX, 'shutdown', 0],
            ];
            $on = static fn (string $f) => in_array($f, self::$features, true);
            if ($on('queries')) {
                $hooks[] = ['query', PHP_INT_MAX, 'onQuery', 1];
            }
            if ($on('errors')) {
                $hooks[] = ['wp_error_added', 10, 'wpError', 4];
            }
            if ($on('notices')) {
                $hooks[] = ['doing_it_wrong_run', 10, 'doingItWrong', 3];
                $hooks[] = ['deprecated_function_run', 10, 'deprecated', 3];
                $hooks[] = ['deprecated_argument_run', 10, 'deprecated', 3];
                $hooks[] = ['deprecated_hook_run', 10, 'deprecated', 4];
                $hooks[] = ['deprecated_file_included', 10, 'deprecated', 4];
            }
            if ($on('mail')) {
                $hooks[] = ['pre_wp_mail', PHP_INT_MAX, 'mail', 2];
                $hooks[] = ['wp_mail_failed', 10, 'mailFailed', 1];
            }
            if ($on('http')) {
                $hooks[] = ['http_api_debug', 10, 'http', 5];
            }
            if ($on('rest')) {
                $hooks[] = ['rest_post_dispatch', PHP_INT_MAX, 'rest', 3];
            }
            if ($on('browser')) {
                foreach (['wp_head', 'admin_head', 'login_head'] as $h) {
                    $hooks[] = [$h, 1, 'browserTag', 0];
                }
            }
            if ($on('editor')) {
                $hooks[] = ['enqueue_block_editor_assets', 10, 'editor', 0];
            }
            if ($on('requests')) {
                $hooks[] = ['template_include', PHP_INT_MAX, 'onTemplate', 1];
            }
            if ($on('hooks')) {
                $hooks[] = ['all', 10, 'anyHook', 1];
            }
            foreach ($hooks as [$name, $priority, $method, $args]) {
                $GLOBALS['wp_filter'][$name][$priority][] = ['function' => [self::class, $method], 'accepted_args' => $args];
            }
            // Nothing leaves the process until boot() knows this is a development site.
            Transport::hold();
            if ($on('errors')) {
                self::catchWarnings();
            }
            if ($on('requests')) {
                $method = $_SERVER['REQUEST_METHOD'] ?? 'GET';
                $uri = $_SERVER['REQUEST_URI'] ?? '/';
                Transport::emitAt('core.log', ['message' => "{$method} {$uri}", 'fields' => self::requestFields()], ['label' => 'request'], self::entrySite());
            }
        } catch (Throwable) {
            // never break the site
        }
    }

    /** At muplugins_loaded (wp-config.php is loaded): only development sites report. */
    public static function boot(): void
    {
        try {
            $dev = self::developmentSite();
            Transport::release($dev);
            if (!$dev) {
                self::$off = true;
                Glint::configure(enabled: false);
            }
        } catch (Throwable) {
            self::$off = true;
        }
    }

    /** WP_ENVIRONMENT_TYPE local or development, WP_DEBUG on, or GLINT_ENABLED=1. */
    private static function developmentSite(): bool
    {
        $forced = strtolower(trim((string) Transport::env('GLINT_ENABLED')));
        if (in_array($forced, ['1', 'true', 'on'], true)) {
            return true;
        }
        if (function_exists('wp_get_environment_type')) {
            $env = (string) wp_get_environment_type();
        } else {
            $env = defined('WP_ENVIRONMENT_TYPE') ? (string) constant('WP_ENVIRONMENT_TYPE') : (string) (getenv('WP_ENVIRONMENT_TYPE') ?: 'production');
        }
        if (in_array($env, ['local', 'development'], true)) {
            return true;
        }
        return defined('WP_DEBUG') && (bool) constant('WP_DEBUG');
    }

    private static function looksLikeWordPress(): bool
    {
        if (PHP_SAPI === 'cli' && !self::setting('GLINT_WP_CLI')) {
            return false; // wp-cli and cron scripts: only when asked
        }
        $dir = dirname((string) ($_SERVER['SCRIPT_FILENAME'] ?? ''));
        for ($i = 0; $i < 3 && $dir !== '' && $dir !== '/' && $dir !== '.'; $i++) {
            if (is_file($dir . '/wp-load.php') && is_file($dir . '/wp-settings.php')) {
                return true;
            }
            $dir = dirname($dir);
        }
        return false;
    }

    private static function setting(string $name): ?string
    {
        if (defined($name)) {
            return (string) constant($name);
        }
        $v = getenv($name);
        return $v === false ? null : $v;
    }

    /** @return array<string, mixed> */
    private static function requestFields(): array
    {
        $script = basename((string) ($_SERVER['SCRIPT_NAME'] ?? ''));
        $f = ['url' => (($_SERVER['HTTPS'] ?? '') === 'on' ? 'https' : 'http') . '://' . ($_SERVER['HTTP_HOST'] ?? '') . ($_SERVER['REQUEST_URI'] ?? '')];
        if ($script === 'admin-ajax.php') {
            $f['ajax'] = $_REQUEST['action'] ?? null;
        }
        if (str_contains((string) ($_SERVER['REQUEST_URI'] ?? ''), '/wp-json/') || isset($_GET['rest_route'])) {
            $f['rest'] = true;
        }
        if ($script === 'wp-cron.php') {
            $f['cron'] = true;
        }
        return $f;
    }

    /** @return array{file?: string, line?: int} */
    private static function entrySite(): array
    {
        $file = (string) ($_SERVER['SCRIPT_FILENAME'] ?? '');
        return $file === '' ? [] : ['file' => $file, 'line' => 1];
    }

    // ------------------------------------------------------------------ SQL

    /** `query` filter: runs before each query; the previous one has finished by now. */
    public static function onQuery(mixed $sql): mixed
    {
        if (self::$off || !is_string($sql)) {
            return $sql;
        }
        try {
            self::finishQuery();
            self::$pending = ['sql' => $sql, 'site' => self::themeOrPluginSite()];
        } catch (Throwable) {
        }
        return $sql;
    }

    private static function finishQuery(): void
    {
        $q = self::$pending;
        if ($q === null) {
            return;
        }
        self::$pending = null;
        self::$queries++;
        $db = $GLOBALS['wpdb'] ?? null;
        $payload = ['sql' => $q['sql'], 'conn' => 'wpdb'];
        $meta = [];
        if (is_object($db)) {
            if (preg_match('/^\s*(select|show|describe)/i', $q['sql'])) {
                $payload['rows'] = (int) ($db->num_rows ?? 0);
            } else {
                $payload['rows'] = (int) ($db->rows_affected ?? 0);
            }
            $last = is_array($db->queries ?? null) ? end($db->queries) : null;
            if (is_array($last) && ($last[0] ?? null) === $q['sql']) {
                $payload['duration_ms'] = round((float) $last[1] * 1000, 3);
            }
            if (!empty($db->last_error)) {
                $payload['error'] = trim((string) preg_replace('/\s+/', ' ', html_entity_decode(strip_tags((string) $db->last_error))));
                $meta['level'] = 'error';
            }
        }
        if (isset($q['site']['stack'])) {
            $payload['caller'] = $q['site']['stack'];
            unset($q['site']['stack']);
        }
        Transport::emitAt('sql.query', $payload, $meta, $q['site']);
    }

    /**
     * The theme or plugin line that led here (the first frame in wp-content), else the first frame
     * outside WordPress core and Glint; plus WordPress's own summary of the call chain.
     *
     * @return array<string, mixed>
     */
    private static function themeOrPluginSite(): array
    {
        $bt = debug_backtrace(DEBUG_BACKTRACE_IGNORE_ARGS, 40);
        $site = [];
        foreach ($bt as $i => $frame) {
            $file = $frame['file'] ?? '';
            if ($file !== '' && str_contains($file, '/wp-content/') && !str_contains($file, '/wp-content/db.php') && !str_contains($file, 'sqlite-database-integration')) {
                $caller = $bt[$i + 1] ?? [];
                $site = ['file' => $file, 'line' => $frame['line'] ?? 0, 'fn' => isset($caller['function']) ? (($caller['class'] ?? '') . ($caller['type'] ?? '') . $caller['function']) : null];
                break;
            }
        }
        if (function_exists('wp_debug_backtrace_summary')) {
            $chain = wp_debug_backtrace_summary(null, 0, false);
            $chain = array_values(array_filter($chain, static fn ($f) => !str_contains((string) $f, 'Glint\\') && !str_contains((string) $f, 'WP_Hook->') && !str_contains((string) $f, 'apply_filters') && !str_contains((string) $f, 'wpdb->')));
            $site['stack'] = implode(' → ', array_slice($chain, -6));
        }
        return $site;
    }

    // ------------------------------------------------------------------ errors and notices

    /** `wp_error_added`: a WP_Error got an error. */
    public static function wpError(mixed $code, mixed $message, mixed $data, mixed $error = null): void
    {
        // Failed mail and HTTP requests have their own events.
        if (self::$off || in_array($code, ['wp_mail_failed', 'http_request_failed'], true)) {
            return;
        }
        // WordPress copies errors around (merging, REST responses): report each once.
        $key = (is_scalar($code) ? (string) $code : '') . "\0" . (is_scalar($message) ? (string) $message : '');
        if (isset(self::$seenErrors[$key])) {
            return;
        }
        self::$seenErrors[$key] = true;
        try {
            $site = self::themeOrPluginSite();
            $stack = $site['stack'] ?? null;
            unset($site['stack']);
            $payload = ['code' => is_scalar($code) ? (string) $code : 'error', 'message' => is_scalar($message) ? (string) $message : '', 'caller' => $stack];
            if ($data !== null && $data !== '') {
                $payload['data'] = Encoder::encode($data);
            }
            Transport::emitAt('x-wp.error', $payload, ['level' => 'warn', 'label' => 'WP_Error'], $site);
        } catch (Throwable) {
        }
    }

    public static function doingItWrong(mixed $function, mixed $message, mixed $version): void
    {
        self::notice('doing it wrong', "{$function}(): " . strip_tags((string) $message), (string) $version);
    }

    public static function deprecated(mixed $what, mixed $replacement = null, mixed $version = null, mixed $message = null): void
    {
        $text = "{$what} is deprecated" . ($version ? " since {$version}" : '') . ($replacement && is_string($replacement) ? ", use {$replacement}" : '');
        self::notice('deprecated', $text, (string) $version);
    }

    private static function notice(string $label, string $message, string $version): void
    {
        if (self::$off) {
            return;
        }
        try {
            $site = self::themeOrPluginSite();
            unset($site['stack']);
            Transport::emitAt('core.log', ['message' => $message, 'fields' => array_filter(['since' => $version])], ['level' => 'warn', 'label' => $label], $site);
        } catch (Throwable) {
        }
    }

    /** PHP warnings and notices (what WP_DEBUG would print), then PHP's own handling as usual. */
    private static function catchWarnings(): void
    {
        $previous = null;
        $previous = set_error_handler(static function (int $no, string $msg, string $file = '', int $line = 0) use (&$previous): bool {
            // WordPress repeats _doing_it_wrong() and deprecations through trigger_error(); the
            // hooks above already reported them.
            $wpOwn = ($no & (E_USER_NOTICE | E_USER_DEPRECATED | E_USER_WARNING)) && str_contains($file, '/wp-includes/');
            if (!self::$off && !$wpOwn && (error_reporting() & $no)) {
                $deprecation = (bool) ($no & (E_DEPRECATED | E_USER_DEPRECATED));
                // Deprecations from WordPress core itself are not yours to fix.
                if (!$deprecation || str_contains($file, '/wp-content/')) {
                    try {
                        $level = $no & (E_WARNING | E_USER_WARNING | E_USER_ERROR | E_RECOVERABLE_ERROR) ? 'warn' : 'info';
                        Transport::emitAt('core.log', ['message' => $msg], ['level' => $level, 'label' => $deprecation ? 'PHP deprecated' : 'PHP ' . ($level === 'warn' ? 'warning' : 'notice')], ['file' => $file, 'line' => $line]);
                    } catch (Throwable) {
                    }
                }
            }
            return is_callable($previous) ? (bool) $previous($no, $msg, $file, $line) : false;
        }, E_ALL & ~E_STRICT);
    }

    // ------------------------------------------------------------------ mail, HTTP, REST

    /** `pre_wp_mail`: before wp_mail() sends; returning null lets it send as usual. */
    public static function mail(mixed $short, mixed $atts): mixed
    {
        if (self::$off || !is_array($atts)) {
            return $short;
        }
        try {
            $site = self::themeOrPluginSite();
            unset($site['stack']);
            $message = (string) ($atts['message'] ?? '');
            Transport::emitAt('x-wp.mail', [
                'to' => $atts['to'] ?? null,
                'subject' => (string) ($atts['subject'] ?? ''),
                'message' => strlen($message) > 20000 ? substr($message, 0, 20000) . '…' : $message,
                'headers' => $atts['headers'] ?? null,
                'attachments' => $atts['attachments'] ?? null,
            ], ['label' => 'mail'], $site);
        } catch (Throwable) {
        }
        return $short;
    }

    public static function mailFailed(mixed $error): void
    {
        if (self::$off || !is_object($error) || !method_exists($error, 'get_error_message')) {
            return;
        }
        try {
            Transport::emitAt('x-wp.mail', ['failed' => true, 'error' => $error->get_error_message(), 'data' => Encoder::encode($error->get_error_data())], ['level' => 'error', 'label' => 'mail failed'], []);
        } catch (Throwable) {
        }
    }

    /** `http_api_debug`: after each wp_remote_*() request. */
    public static function http(mixed $response, mixed $context, mixed $class, mixed $args, mixed $url): void
    {
        if (self::$off || $context !== 'response') {
            return;
        }
        try {
            $method = strtoupper((string) (is_array($args) ? ($args['method'] ?? 'GET') : 'GET'));
            $payload = ['method' => $method, 'url' => (string) $url];
            $level = 'info';
            if (is_object($response) && method_exists($response, 'get_error_message')) {
                $payload['error'] = $response->get_error_message();
                $level = 'error';
            } elseif (is_array($response)) {
                $status = (int) ($response['response']['code'] ?? 0);
                $payload['status'] = $status;
                $level = $status >= 500 ? 'error' : ($status >= 400 ? 'warn' : 'info');
            }
            $site = self::themeOrPluginSite();
            unset($site['stack']);
            Transport::emitAt('x-wp.http', $payload, ['level' => $level, 'label' => 'HTTP'], $site);
        } catch (Throwable) {
        }
    }

    /** `rest_post_dispatch`: REST responses with an error status (the block editor's saves, too). */
    public static function rest(mixed $result, mixed $server = null, mixed $request = null): mixed
    {
        if (self::$off || !is_object($result) || !method_exists($result, 'get_status')) {
            return $result;
        }
        try {
            $status = (int) $result->get_status();
            if ($status >= 400) {
                $data = method_exists($result, 'get_data') ? $result->get_data() : null;
                $payload = [
                    'status' => $status,
                    'method' => is_object($request) && method_exists($request, 'get_method') ? $request->get_method() : null,
                    'route' => is_object($request) && method_exists($request, 'get_route') ? $request->get_route() : null,
                    'code' => is_array($data) ? ($data['code'] ?? null) : null,
                    'message' => is_array($data) ? ($data['message'] ?? null) : null,
                    'data' => Encoder::encode($data),
                ];
                Transport::emitAt('x-wp.rest', $payload, ['level' => $status >= 500 ? 'error' : 'warn', 'label' => 'REST'], []);
            }
        } catch (Throwable) {
        }
        return $result;
    }

    // ------------------------------------------------------------------ browser and block editor

    /** wp_head, admin_head, login_head: the browser script, so JavaScript errors arrive too. */
    public static function browserTag(): void
    {
        static $done = false;
        if (self::$off || $done) {
            return;
        }
        $done = true;
        $project = self::projectName();
        echo '<script crossorigin="anonymous" src="' . htmlspecialchars(Transport::url() . '/v1/browser.js', ENT_QUOTES) . '" data-project="' . htmlspecialchars($project, ENT_QUOTES) . '"></script>' . "\n";
    }

    /** Block editor: invalid blocks and error notices ("Updating failed", …) as they appear. */
    public static function editor(): void
    {
        if (self::$off || !function_exists('wp_register_script')) {
            return;
        }
        wp_register_script('glint-editor', false, ['wp-data', 'wp-blocks'], null, true);
        wp_enqueue_script('glint-editor');
        wp_add_inline_script('glint-editor', <<<'JS'
(function () {
  if (!window.wp || !wp.data || !window.glint) return;
  var notices = new Set(), blocks = new Set(), timer = null;
  function strip(s) { return String(s || "").replace(/<[^>]+>/g, ""); }
  function check() {
    timer = null;
    try {
      var n = wp.data.select("core/notices");
      (n ? n.getNotices() : []).forEach(function (x) {
        if (x.status === "error" && !notices.has(x.id)) {
          notices.add(x.id);
          glint.log("Editor: " + strip(x.content), { notice: x.id, context: "block editor" }, "error");
        }
      });
      var be = wp.data.select("core/block-editor");
      var walk = function (list) {
        (list || []).forEach(function (b) {
          if (b.isValid === false && !blocks.has(b.clientId)) {
            blocks.add(b.clientId);
            glint.log("Invalid block: " + b.name, {
              block: b.name,
              attributes: b.attributes,
              saved: String(b.originalContent || "").slice(0, 2000),
              issues: (b.validationIssues || []).map(function (i) { return i && i.args ? (window.__glint && __glint.format ? __glint.format(i.args) : i.args.join(" ")) : String(i); }),
            }, "error");
          }
          walk(b.innerBlocks);
        });
      };
      if (be) walk(be.getBlocks());
    } catch (e) {}
  }
  wp.data.subscribe(function () { if (!timer) timer = setTimeout(check, 400); });
})();
JS);
    }

    /** The project name glintd would give this site: the git repository's, else the folder's. */
    private static function projectName(): string
    {
        $dir = defined('ABSPATH') ? rtrim((string) constant('ABSPATH'), '/') : getcwd();
        for ($d = (string) $dir; $d !== '' && $d !== '/'; $d = dirname($d)) {
            if (is_dir($d . '/.git')) {
                return basename($d);
            }
            if (is_file($d . '/.git') && preg_match('#gitdir:\s*(.+)#', (string) file_get_contents($d . '/.git'), $m)) {
                $git = trim($m[1]);
                if (preg_match('#^(.*)/\.git/worktrees/#', $git, $r)) {
                    return basename($r[1]);
                }
                return basename($d);
            }
        }
        return basename((string) $dir);
    }

    // ------------------------------------------------------------------ the request summary

    public static function onTemplate(mixed $template): mixed
    {
        if (is_string($template)) {
            self::$template = $template;
        }
        return $template;
    }

    public static function anyHook(mixed $name): void
    {
        if (is_string($name)) {
            self::$hookCounts[$name] = (self::$hookCounts[$name] ?? 0) + 1;
        }
    }

    public static function shutdown(): void
    {
        try {
            self::finishQuery();
            if (self::$off) {
                return;
            }
            if (in_array('requests', self::$features, true)) {
                $q = $GLOBALS['wp_query'] ?? null;
                $summary = [
                    'status' => http_response_code() ?: null,
                    'duration_ms' => round((microtime(true) - self::$t0) * 1000, 1),
                    'queries' => self::$queries,
                    'memory_mb' => round(memory_get_peak_usage(true) / 1048576, 1),
                    'template' => self::$template,
                    'conditionals' => self::conditionalsNow(),
                    'user' => function_exists('get_current_user_id') ? get_current_user_id() : null,
                ];
                if (is_object($q) && isset($q->query_vars)) {
                    $summary['query'] = array_filter((array) $q->query, static fn ($v) => $v !== '' && $v !== null);
                    $summary['found_posts'] = (int) ($q->found_posts ?? 0);
                }
                Transport::emitAt('x-wp.request', array_filter($summary, static fn ($v) => $v !== null && $v !== []), ['label' => 'WordPress'], self::$template ? ['file' => self::$template, 'line' => 1] : []);
            }
            if (self::$hookCounts) {
                arsort(self::$hookCounts);
                $rows = [];
                foreach (array_slice(self::$hookCounts, 0, 100, true) as $name => $n) {
                    $rows[] = ['hook' => $name, 'fired' => $n];
                }
                Transport::emitAt('core.table', ['rows' => $rows, 'columns' => ['hook', 'fired']], ['label' => 'hooks fired'], []);
            }
        } catch (Throwable) {
        }
    }

    /** @return list<string> */
    private static function conditionalsNow(): array
    {
        $names = ['is_front_page', 'is_home', 'is_singular', 'is_single', 'is_page', 'is_archive', 'is_category', 'is_tag', 'is_tax', 'is_author', 'is_search', 'is_404', 'is_admin', 'is_feed', 'wp_doing_ajax', 'wp_is_json_request', 'wp_doing_cron'];
        $true = [];
        foreach ($names as $fn) {
            try {
                if (function_exists($fn) && (!str_starts_with($fn, 'is_') || isset($GLOBALS['wp_query']) || $fn === 'is_admin') && $fn()) {
                    $true[] = $fn;
                }
            } catch (Throwable) {
            }
        }
        return $true;
    }

    // ------------------------------------------------------------------ methods for your code

    /** A post with its fields, meta, terms and permalink. */
    public static function post(mixed $post = null): Entry
    {
        $p = function_exists('get_post') ? get_post($post) : null;
        if (!$p) {
            return Glint::values(['post' => $post, 'found' => false]);
        }
        $terms = [];
        foreach (get_object_taxonomies($p->post_type) as $tax) {
            $t = wp_get_post_terms($p->ID, $tax, ['fields' => 'names']);
            if (is_array($t) && $t) {
                $terms[$tax] = $t;
            }
        }
        return Glint::values(['post' => $p->to_array(), 'meta' => array_map(static fn ($v) => count($v) === 1 ? maybe_unserialize($v[0]) : array_map('maybe_unserialize', $v), get_post_meta($p->ID)), 'terms' => $terms, 'permalink' => get_permalink($p)])->label("post {$p->ID}: {$p->post_title}");
    }

    /** A user (default: the current one) with roles, capabilities that are on, and meta. */
    public static function user(mixed $user = null): Entry
    {
        $u = $user === null ? wp_get_current_user() : (is_object($user) ? $user : get_user_by(is_numeric($user) ? 'id' : 'login', $user));
        if (!$u || !$u->ID) {
            return Glint::values(['user' => $user, 'logged_in' => false])->label('no user');
        }
        $data = (array) $u->data;
        unset($data['user_pass'], $data['user_activation_key']);
        return Glint::values(['user' => $data, 'roles' => $u->roles, 'caps' => array_keys(array_filter($u->allcaps)), 'meta' => array_map(static fn ($v) => count($v) === 1 ? maybe_unserialize($v[0]) : $v, array_diff_key(get_user_meta($u->ID), ['session_tokens' => 1]))])->label("user {$u->ID}: {$u->user_login}");
    }

    /** A WP_Query (default: the main one): its vars, the SQL it ran, and what it found. */
    public static function query(mixed $query = null): Entry
    {
        $q = $query ?? ($GLOBALS['wp_query'] ?? null);
        if (!is_object($q)) {
            return Glint::values(['query' => null]);
        }
        $posts = array_map(static fn ($p) => is_object($p) ? ['ID' => $p->ID, 'title' => $p->post_title, 'type' => $p->post_type, 'status' => $p->post_status] : $p, (array) ($q->posts ?? []));
        return Glint::values(['vars' => array_filter((array) $q->query_vars, static fn ($v) => $v !== '' && $v !== [] && $v !== null && $v !== false && $v !== 0), 'sql' => $q->request ?? null, 'found_posts' => $q->found_posts ?? null, 'max_num_pages' => $q->max_num_pages ?? null, 'posts' => $posts])->label('WP_Query');
    }

    /** Who listens to a hook: priority, callback, and the file and line it is defined in. */
    public static function hooks(string $name): Entry
    {
        $rows = [];
        $hook = $GLOBALS['wp_filter'][$name] ?? null;
        foreach ((is_object($hook) ? $hook->callbacks : (array) $hook) as $priority => $callbacks) {
            foreach ((array) $callbacks as $cb) {
                $fn = $cb['function'] ?? null;
                [$label, $where] = self::describe($fn);
                $rows[] = ['priority' => $priority, 'callback' => $label, 'args' => $cb['accepted_args'] ?? 1, 'defined at' => $where];
            }
        }
        return Glint::table($rows, ['priority', 'callback', 'args', 'defined at'])->label("hook {$name}" . ($rows ? '' : ' (nothing attached)'));
    }

    /** @return array{0: string, 1: string} */
    private static function describe(mixed $fn): array
    {
        try {
            if (is_string($fn) && str_contains($fn, '::')) {
                $fn = explode('::', $fn, 2);
            }
            if (is_array($fn) && count($fn) === 2) {
                $r = new \ReflectionMethod($fn[0], (string) $fn[1]);
                $label = (is_object($fn[0]) ? get_class($fn[0]) . '->' : $fn[0] . '::') . $fn[1];
            } elseif ($fn instanceof \Closure || is_string($fn)) {
                $r = new \ReflectionFunction($fn);
                $label = $fn instanceof \Closure ? 'closure' : $fn;
            } elseif (is_object($fn) && method_exists($fn, '__invoke')) {
                $r = new \ReflectionMethod($fn, '__invoke');
                $label = get_class($fn) . '->__invoke';
            } else {
                return [is_scalar($fn) ? (string) $fn : gettype($fn), '?'];
            }
            $file = $r->getFileName();
            return [$label, $file ? $file . ':' . $r->getStartLine() : 'built in'];
        } catch (Throwable) {
            return ['?', '?'];
        }
    }

    /** The template file this request uses, and the hierarchy of templates WordPress looked for. */
    public static function template_(): Entry
    {
        return Glint::values(['template' => self::$template, 'theme' => function_exists('get_stylesheet') ? get_stylesheet() : null, 'block_theme' => function_exists('wp_is_block_theme') ? wp_is_block_theme() : null])->label('template');
    }

    /** Which conditional tags are true right now (is_single, is_admin, …). */
    public static function conditionals(): Entry
    {
        return Glint::values(self::conditionalsNow())->label('conditionals');
    }

    /** Options by name, unserialized. */
    public static function options(string ...$names): Entry
    {
        $out = [];
        foreach ($names as $n) {
            $out[$n] = get_option($n);
        }
        return Glint::values($out)->label('options');
    }

    /** All queries of this request so far, as a table (timings need SAVEQUERIES). */
    public static function queries(): Entry
    {
        $db = $GLOBALS['wpdb'] ?? null;
        $rows = [];
        foreach ((array) ($db->queries ?? []) as $q) {
            $rows[] = ['sql' => $q[0] ?? '', 'ms' => isset($q[1]) ? round((float) $q[1] * 1000, 2) : null, 'caller' => $q[2] ?? ''];
        }
        return Glint::table($rows ?: [['sql' => 'define SAVEQUERIES in wp-config.php to list queries here; each one is already sent as it runs', 'ms' => null, 'caller' => '']], ['sql', 'ms', 'caller'])->label('queries');
    }
}
