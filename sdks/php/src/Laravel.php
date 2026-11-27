<?php

declare(strict_types=1);

namespace Glint;

use Throwable;

/**
 * Laravel, with no package and no code: `glint install` prepends Glint to every PHP process, and
 * when it runs inside a Laravel app (public/index.php, `artisan serve`, artisan commands, queue
 * workers) this class hooks into the application as it boots.
 *
 * How it gets in: Laravel defines its env() helper only when no env() exists yet, and calls it
 * while the configuration loads, after the application object exists and before any provider
 * runs. src/laravel-env.php defines an identical env() first; its first call hands the
 * application to attach(). With a cached configuration (`php artisan config:cache`) env() is not
 * called, so require glint/glint with Composer instead: Laravel then discovers
 * LaravelServiceProvider, which calls attach() too.
 *
 * What it reports:
 * - each HTTP request as one run with one root event (a core.measure): method, route, controller
 *   action, views, status, time, memory, number of queries; everything else nests under it;
 * - every query (sql.query) with its bindings, time and the app line that ran it;
 * - exceptions the exception handler reports, with the request they happened in;
 * - everything written with Log::, at its level;
 * - failed calls of the HTTP client (Http::get() and friends): 4xx, 5xx and connection errors;
 * - queue jobs: in a worker each job is a run of its own, with the job as root.
 *
 * GLINT_LARAVEL=off (environment) turns it off. An app with APP_ENV=production is left alone.
 */
final class Laravel
{
    private static bool $preloaded = false;
    private static bool $attached = false;
    private static bool $off = false;
    private static ?string $base = null;
    private static float $t0 = 0.0;
    private static ?string $root = null;
    private static int $queries = 0;
    private static float $sqlMs = 0.0;
    /** @var array<string, mixed> */
    private static array $route = [];
    private static ?int $status = null;
    /** @var array<string, string> view name => file */
    private static array $views = [];
    /** @var array<int, true> spl_object_id of exceptions already sent */
    private static array $reported = [];
    /** @var list<array{root: string, run: ?string, t0: float, queries: int, name: string}> */
    private static array $jobs = [];

    /**
     * Called by the prepend file when the script belongs to a Laravel app at $base. Defines the
     * env() hook; nothing else happens until Laravel calls it.
     */
    public static function preload(string $base): void
    {
        try {
            if (self::$preloaded || self::disabledByEnv()) {
                return;
            }
            if (PHP_SAPI === 'cli-server') {
                // `artisan serve`: files in public/ are served as they are, not by Laravel.
                $path = (string) parse_url((string) ($_SERVER['REQUEST_URI'] ?? '/'), PHP_URL_PATH);
                if ($path !== '/' && is_file($base . '/public' . urldecode($path))) {
                    return;
                }
            }
            self::$preloaded = true;
            self::$base = $base;
            self::$t0 = (float) ($_SERVER['REQUEST_TIME_FLOAT'] ?? microtime(true));
            require_once __DIR__ . '/laravel-env.php';
        } catch (Throwable) {
            // never break the app
        }
    }

    /** First env() calls: attaches once the application exists. */
    public static function fromEnv(): void
    {
        if (self::$attached || self::$off) {
            return;
        }
        try {
            $container = 'Illuminate\Container\Container';
            if (!class_exists($container, false)) {
                return;
            }
            // Container::getInstance() would create one as a side effect; read the slot instead.
            $prop = new \ReflectionProperty($container, 'instance');
            $prop->setAccessible(true);
            $app = $prop->getValue();
            if (is_object($app) && is_a($app, 'Illuminate\Foundation\Application')) {
                self::attach($app);
            }
        } catch (Throwable) {
            self::$off = true;
        }
    }

    /** Hooks into a Laravel application. Safe to call more than once. */
    public static function attach(object $app): void
    {
        if (self::$attached) {
            return;
        }
        self::$attached = true;
        try {
            if (self::disabledByEnv() || self::envValue('APP_ENV') === 'production') {
                self::$off = true;
                return;
            }
            self::$base ??= method_exists($app, 'basePath') ? $app->basePath() : null;
            if (self::$t0 === 0.0) {
                self::$t0 = (float) ($_SERVER['REQUEST_TIME_FLOAT'] ?? microtime(true));
            }
            if (PHP_SAPI !== 'cli') {
                self::$root = Transport::newId();
                if (self::$root !== null) {
                    Transport::pushScope(self::$root);
                }
                register_shutdown_function([self::class, 'shutdown']);
            }

            $events = $app['events'];
            $events->listen('Illuminate\Database\Events\QueryExecuted', [self::class, 'onQuery']);
            $events->listen('Illuminate\Log\Events\MessageLogged', [self::class, 'onLog']);
            $events->listen('Illuminate\Routing\Events\RouteMatched', [self::class, 'onRoute']);
            $events->listen('Illuminate\Foundation\Http\Events\RequestHandled', [self::class, 'onHandled']);
            $events->listen('composing: *', [self::class, 'onView']);
            $events->listen('Illuminate\Http\Client\Events\ResponseReceived', [self::class, 'onHttpResponse']);
            $events->listen('Illuminate\Http\Client\Events\ConnectionFailed', [self::class, 'onHttpFailed']);
            $events->listen('Illuminate\Queue\Events\JobProcessing', [self::class, 'onJobStart']);
            $events->listen('Illuminate\Queue\Events\JobProcessed', [self::class, 'onJobDone']);
            $events->listen('Illuminate\Queue\Events\JobFailed', [self::class, 'onJobDone']);

            // `php artisan serve`: the server it starts must load Glint (LaravelServeCommand).
            $serve = 'Illuminate\Foundation\Console\ServeCommand';
            if (PHP_SAPI === 'cli' && ($_SERVER['argv'][1] ?? null) === 'serve' && class_exists($serve)) {
                $app->extend($serve, static fn ($command) => get_class($command) === $serve ? new LaravelServeCommand() : $command);
            }

            $handler = 'Illuminate\Contracts\Debug\ExceptionHandler';
            $hook = static function (mixed $h): void {
                if (is_object($h) && method_exists($h, 'reportable')) {
                    $h->reportable(static function (Throwable $e): void {
                        Laravel::report($e);
                    });
                }
            };
            if (method_exists($app, 'resolved') && $app->resolved($handler)) {
                $hook($app->make($handler));
            } else {
                $app->afterResolving($handler, $hook);
            }
        } catch (Throwable) {
            self::$off = true;
        }
    }

    private static function disabledByEnv(): bool
    {
        $v = getenv('GLINT_LARAVEL');
        return $v !== false && in_array(strtolower($v), ['0', 'off', 'false', 'no'], true);
    }

    private static function envValue(string $key): ?string
    {
        try {
            if (class_exists('Illuminate\Support\Env')) {
                $v = \Illuminate\Support\Env::get($key);
                return $v === null ? null : (string) $v;
            }
        } catch (Throwable) {
        }
        $v = getenv($key);
        return $v === false ? null : $v;
    }

    // ------------------------------------------------------------------ SQL

    public static function onQuery(object $q): void
    {
        if (self::$off) {
            return;
        }
        try {
            self::$queries++;
            $ms = (float) ($q->time ?? 0);
            self::$sqlMs += $ms;
            if (self::$jobs !== []) {
                self::$jobs[count(self::$jobs) - 1]['queries']++;
            }
            $payload = [
                'sql' => (string) ($q->sql ?? ''),
                'duration_ms' => round($ms, 3),
                'conn' => (string) ($q->connectionName ?? ''),
            ];
            $bindings = (array) ($q->bindings ?? []);
            if ($bindings !== []) {
                $payload['bindings'] = array_map(static fn ($b) => $b instanceof \DateTimeInterface ? $b->format('Y-m-d H:i:s') : Encoder::encode($b), array_values($bindings));
            }
            [$site, $caller] = self::appSite();
            if ($caller !== '') {
                $payload['caller'] = $caller;
            }
            Transport::emitAt('sql.query', $payload, [], $site);
        } catch (Throwable) {
        }
    }

    /**
     * The app line that led here (the first frame outside vendor/ and Glint), and a short chain
     * of the app functions above it.
     *
     * @return array{0: array<string, mixed>, 1: string}
     */
    private static function appSite(): array
    {
        $bt = debug_backtrace(DEBUG_BACKTRACE_IGNORE_ARGS, 60);
        $site = [];
        $chain = [];
        foreach ($bt as $i => $frame) {
            $file = $frame['file'] ?? '';
            if ($file === '' || str_contains($file, '/vendor/') || str_starts_with($file, __DIR__ . '/') || str_contains($file, '/storage/framework/views/') && $site !== []) {
                continue;
            }
            $caller = $bt[$i + 1] ?? [];
            $fn = isset($caller['function']) ? (($caller['class'] ?? '') . ($caller['type'] ?? '') . $caller['function']) : null;
            if ($site === []) {
                $site = ['file' => self::viewSource($file), 'line' => (int) ($frame['line'] ?? 0), 'fn' => $fn];
            }
            if ($fn !== null && count($chain) < 4) {
                $chain[] = $fn;
            }
        }
        return [$site, implode(' ← ', $chain)];
    }

    /** A compiled Blade view names its template in a trailing comment: point at the template. */
    private static function viewSource(string $file): string
    {
        if (!str_contains($file, '/storage/framework/views/')) {
            return $file;
        }
        $tail = @file_get_contents($file, false, null, max(0, (int) @filesize($file) - 400));
        if (is_string($tail) && preg_match('#/\*\*PATH (.+?) ENDPATH\*\*/#', $tail, $m)) {
            return $m[1];
        }
        return $file;
    }

    // ------------------------------------------------------------------ logs and exceptions

    public static function onLog(object $e): void
    {
        if (self::$off) {
            return;
        }
        try {
            $context = (array) ($e->context ?? []);
            $ex = $context['exception'] ?? null;
            if ($ex instanceof Throwable) {
                // The exception handler logs what it reports: one event, not two.
                self::report($ex);
                return;
            }
            $level = match ((string) ($e->level ?? 'info')) {
                'emergency', 'alert', 'critical' => 'fatal',
                'error' => 'error',
                'warning' => 'warn',
                'debug' => 'debug',
                default => 'info',
            };
            $payload = ['message' => is_scalar($e->message ?? null) ? (string) $e->message : json_encode($e->message)];
            if ($context !== []) {
                $payload['fields'] = Encoder::encode($context);
            }
            [$site] = self::appSite();
            Transport::emitAt('core.log', $payload, ['level' => $level, 'label' => 'Log'], $site);
        } catch (Throwable) {
        }
    }

    /** Sends $e once, with the request (or job) it happened in. */
    public static function report(Throwable $e): void
    {
        if (self::$off || isset(self::$reported[spl_object_id($e)])) {
            return;
        }
        self::$reported[spl_object_id($e)] = true;
        try {
            $payload = Glint::exceptionData($e);
            $context = self::requestContext();
            if (self::$jobs !== []) {
                $context['job'] = self::$jobs[count(self::$jobs) - 1]['name'];
            }
            if ($context !== []) {
                $payload['context'] = $context;
            }
            $site = [];
            foreach ($payload['frames'] as $f) {
                if (!($f['vendor'] ?? false) && isset($f['file'])) {
                    $site = ['file' => self::viewSource($f['file']), 'line' => $f['line'] ?? 0, 'fn' => $f['fn'] ?? null];
                    break;
                }
            }
            Transport::emitAt('core.exception', $payload, ['level' => 'error', 'label' => 'Laravel'], $site);
        } catch (Throwable) {
        }
    }

    /** @return array<string, mixed> */
    private static function requestContext(): array
    {
        if (PHP_SAPI === 'cli') {
            return [];
        }
        $c = [
            'method' => $_SERVER['REQUEST_METHOD'] ?? null,
            'url' => self::url(),
            'route' => self::$route['uri'] ?? null,
            'action' => self::$route['action'] ?? null,
        ];
        $input = $_POST + $_GET;
        if ($input !== []) {
            foreach ($input as $k => $v) {
                if (preg_match('/pass|token|secret|key|card|cvv/i', (string) $k)) {
                    $input[$k] = '[hidden]';
                }
            }
            $c['input'] = Encoder::encode($input);
        }
        return array_filter($c, static fn ($v) => $v !== null);
    }

    private static function url(): string
    {
        return ((($_SERVER['HTTPS'] ?? '') === 'on') ? 'https' : 'http') . '://' . ($_SERVER['HTTP_HOST'] ?? 'localhost') . ($_SERVER['REQUEST_URI'] ?? '/');
    }

    // ------------------------------------------------------------------ the request

    public static function onRoute(object $e): void
    {
        try {
            $r = $e->route ?? null;
            if (!is_object($r)) {
                return;
            }
            $action = method_exists($r, 'getActionName') ? $r->getActionName() : null;
            self::$route = array_filter([
                'uri' => method_exists($r, 'uri') ? '/' . ltrim($r->uri(), '/') : null,
                'name' => method_exists($r, 'getName') ? $r->getName() : null,
                'action' => $action === 'Closure' ? 'closure' : $action,
            ]);
        } catch (Throwable) {
        }
    }

    public static function onHandled(object $e): void
    {
        try {
            $res = $e->response ?? null;
            if (is_object($res) && method_exists($res, 'getStatusCode')) {
                self::$status = (int) $res->getStatusCode();
            }
        } catch (Throwable) {
        }
    }

    /** Wildcard listener: ('composing: view.name', [$view]). */
    public static function onView(mixed $event, mixed $data = null): void
    {
        try {
            $view = is_array($data) ? ($data[0] ?? null) : null;
            if (is_object($view) && method_exists($view, 'name') && count(self::$views) < 50) {
                self::$views[(string) $view->name()] = method_exists($view, 'getPath') ? (string) $view->getPath() : '';
            }
        } catch (Throwable) {
        }
    }

    public static function shutdown(): void
    {
        if (self::$off || self::$root === null) {
            return;
        }
        try {
            Transport::popScope();
            $status = self::$status ?? (http_response_code() ?: null);
            $method = (string) ($_SERVER['REQUEST_METHOD'] ?? 'GET');
            $path = (string) parse_url((string) ($_SERVER['REQUEST_URI'] ?? '/'), PHP_URL_PATH);
            $action = self::$route['action'] ?? null;
            $short = is_string($action) ? preg_replace('#^App\\\\Http\\\\Controllers\\\\#', '', $action) : null;
            $name = trim("{$method} " . (self::$route['uri'] ?? $path) . ($status ? " → {$status}" : '') . ($short ? " · {$short}" : ''));
            $payload = array_filter([
                'name' => $name,
                'duration_ms' => round((microtime(true) - self::$t0) * 1000, 1),
                'memory_bytes' => memory_get_peak_usage(true),
                'method' => $method,
                'url' => self::url(),
                'route' => self::$route['uri'] ?? null,
                'route_name' => self::$route['name'] ?? null,
                'action' => $action,
                'views' => array_keys(self::$views) ?: null,
                'status' => $status,
                'queries' => self::$queries,
                'sql_ms' => round(self::$sqlMs, 2),
            ], static fn ($v) => $v !== null);
            $meta = ['label' => 'Laravel'];
            if ($status !== null && $status >= 500) {
                $meta['level'] = 'error';
            }
            $site = self::actionSite($action) ?? ['file' => self::$base ? self::$base . '/routes/web.php' : null, 'line' => 1];
            Transport::emitAt('core.measure', $payload, $meta, $site, self::$root);
        } catch (Throwable) {
        }
    }

    /** @return array{file: string, line: int, fn: string}|null the controller method, if any */
    private static function actionSite(mixed $action): ?array
    {
        if (!is_string($action) || !str_contains($action, '@')) {
            return null;
        }
        try {
            [$class, $method] = explode('@', $action, 2);
            $r = new \ReflectionMethod($class, $method);
            $file = $r->getFileName();
            return $file ? ['file' => $file, 'line' => (int) $r->getStartLine(), 'fn' => $action] : null;
        } catch (Throwable) {
            return null;
        }
    }

    // ------------------------------------------------------------------ HTTP client

    public static function onHttpResponse(object $e): void
    {
        if (self::$off) {
            return;
        }
        try {
            $res = $e->response ?? null;
            $status = is_object($res) && method_exists($res, 'status') ? (int) $res->status() : 0;
            if ($status < 400) {
                return;
            }
            $body = method_exists($res, 'body') ? (string) $res->body() : '';
            self::http($e->request ?? null, ['status' => $status, 'body' => strlen($body) > 2000 ? substr($body, 0, 2000) . '…' : $body], $status >= 500 ? 'error' : 'warn');
        } catch (Throwable) {
        }
    }

    public static function onHttpFailed(object $e): void
    {
        if (self::$off) {
            return;
        }
        $ex = $e->exception ?? null;
        self::http($e->request ?? null, ['error' => $ex instanceof Throwable ? $ex->getMessage() : 'connection failed'], 'error');
    }

    /** @param array<string, mixed> $extra */
    private static function http(mixed $req, array $extra, string $level): void
    {
        try {
            $payload = [
                'method' => is_object($req) && method_exists($req, 'method') ? $req->method() : 'GET',
                'url' => is_object($req) && method_exists($req, 'url') ? $req->url() : '',
            ] + $extra;
            [$site] = self::appSite();
            Transport::emitAt('http.request', $payload, ['level' => $level, 'label' => 'HTTP client'], $site);
        } catch (Throwable) {
        }
    }

    // ------------------------------------------------------------------ queue jobs

    public static function onJobStart(object $e): void
    {
        if (self::$off) {
            return;
        }
        try {
            $job = $e->job ?? null;
            $name = is_object($job) && method_exists($job, 'resolveName') ? (string) $job->resolveName() : 'job';
            $root = Transport::newId();
            if ($root === null) {
                return;
            }
            // A worker handles many jobs: each is a run of its own. A sync job inside a request
            // nests under the request instead.
            $run = PHP_SAPI === 'cli' ? Transport::setRun('php-job-' . $root) : null;
            Transport::pushScope($root);
            self::$jobs[] = ['root' => $root, 'run' => $run, 't0' => microtime(true), 'queries' => 0, 'name' => $name, 'cli' => PHP_SAPI === 'cli'];
        } catch (Throwable) {
        }
    }

    public static function onJobDone(object $e): void
    {
        if (self::$off || self::$jobs === []) {
            return;
        }
        try {
            $failed = str_ends_with(get_class($e), 'JobFailed');
            $ex = $e->exception ?? null;
            if ($failed && $ex instanceof Throwable) {
                self::report($ex); // inside the job, before the worker reports it outside
            }
            $j = array_pop(self::$jobs);
            Transport::popScope();
            $job = $e->job ?? null;
            $payload = [
                'name' => 'job ' . $j['name'] . ($failed ? ' → failed' : ''),
                'duration_ms' => round((microtime(true) - $j['t0']) * 1000, 1),
                'memory_bytes' => memory_get_peak_usage(true),
                'job' => $j['name'],
                'connection' => $e->connectionName ?? null,
                'queue' => is_object($job) && method_exists($job, 'getQueue') ? $job->getQueue() : null,
                'attempts' => is_object($job) && method_exists($job, 'attempts') ? $job->attempts() : null,
                'queries' => $j['queries'],
                'status' => $failed ? 'failed' : 'done',
            ];
            $meta = ['label' => 'Laravel job'];
            if ($failed) {
                $meta['level'] = 'error';
            }
            $site = self::classSite($j['name']);
            Transport::emitAt('core.measure', array_filter($payload, static fn ($v) => $v !== null), $meta, $site, $j['root']);
            if ($j['cli']) {
                Transport::flush(1.0);
                Transport::setRun($j['run']);
            }
        } catch (Throwable) {
        }
    }

    /** @return array<string, mixed> the handle() method of a job class */
    private static function classSite(string $class): array
    {
        try {
            if (class_exists($class) && method_exists($class, 'handle')) {
                $r = new \ReflectionMethod($class, 'handle');
                return ['file' => (string) $r->getFileName(), 'line' => (int) $r->getStartLine(), 'fn' => $class . '->handle'];
            }
        } catch (Throwable) {
        }
        return [];
    }
}
