<?php

namespace Glint;

use Throwable;

/**
 * Queue, delivery and call-site capture. Internal: use the Glint facade.
 *
 * PHP has no threads, so everything runs on the caller's thread. Events go into one in-memory
 * queue, which is sent when it reaches BATCH events, when FLUSH_INTERVAL has passed since the last
 * send (checked on each call), and at shutdown. One queue means events arrive in call order.
 */
final class Transport
{
    public const SDK = 'glint-php/0.1.0';

    private const DEFAULT_URL = 'http://127.0.0.1:23600';
    private const BATCH = 100;
    private const MAX_QUEUE = 10000;
    private const FLUSH_INTERVAL = 0.1;
    private const CONNECT_MS = 500;
    private const TIMEOUT_MS = 2000;
    private const RETRY_AFTER = 5.0;
    private const SRC_DIR = __DIR__ . DIRECTORY_SEPARATOR;
    private const JSON_FLAGS = JSON_PRESERVE_ZERO_FRACTION | JSON_UNESCAPED_SLASHES
        | JSON_UNESCAPED_UNICODE | JSON_INVALID_UTF8_SUBSTITUTE;
    private const B32 = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

    private static ?bool $enabled = null;
    private static string $url = self::DEFAULT_URL;
    private static ?string $project = null;
    private static ?string $run = null;
    /** @var array<int, array<string, mixed>> */
    private static array $queue = [];
    private static int $dropped = 0;
    private static float $downUntil = 0.0;
    private static float $lastFlush = 0.0;
    private static bool $shutdownRegistered = false;
    private static bool $warned = false;

    /** Reads GLINT_* once. Later calls do nothing. */
    public static function boot(): void
    {
        if (self::$enabled !== null) {
            return;
        }
        $url = self::env('GLINT_URL');
        if ($url !== null && $url !== '') {
            self::$url = $url;
        }
        $project = self::env('GLINT_PROJECT');
        self::$project = ($project !== null && $project !== '') ? $project : null;
        self::$enabled = self::enabledFromEnv();
    }

    public static function configure(?string $url, ?bool $enabled, ?string $project): void
    {
        self::boot();
        if ($url !== null && $url !== '') {
            self::$url = $url;
        }
        if ($enabled !== null) {
            self::$enabled = $enabled;
        }
        if ($project !== null) {
            self::$project = $project !== '' ? $project : null;
        }
    }

    /** glintd's base URL, without a trailing slash. */
    public static function url(): string
    {
        self::boot();
        return rtrim(self::$url, '/');
    }

    public static function env(string $key): ?string
    {
        $v = getenv($key);
        if ($v === false) {
            $v = $_ENV[$key] ?? $_SERVER[$key] ?? null;
        }
        return $v === null ? null : (string) $v;
    }

    /** Queues one envelope and returns its id, or null when disabled or on any internal failure. */
    /**
     * Ids of the measures being run, innermost last: what is sent inside Glint::measure() names
     * the innermost one as its parent, so a request reads as a call tree.
     *
     * @var list<string>
     */
    private static array $scopes = [];

    /** Runs $fn with $id as the parent of everything it sends. */
    public static function within(string $id, callable $fn): mixed
    {
        self::$scopes[] = $id;
        try {
            return $fn();
        } finally {
            array_pop(self::$scopes);
        }
    }

    /** A fresh event id, or null when sending is off. */
    public static function newId(): ?string
    {
        try {
            self::boot();
            return self::$enabled ? self::ulid() : null;
        } catch (\Throwable) {
            return null;
        }
    }

    /** @var array{file?: string, line?: int, fn?: string}|null where emitAt says the event comes from */
    private static ?array $at = null;

    /**
     * Like emit(), with the call site given instead of found from the stack (integrations report
     * the theme or plugin line that caused something, not their own hook).
     *
     * @param array{file?: string, line?: int, fn?: string}|null $site
     */
    public static function emitAt(string $kind, mixed $payload, array $meta = [], ?array $site = null): ?string
    {
        self::$at = $site;
        try {
            return self::emit($kind, $payload, $meta);
        } finally {
            self::$at = null;
        }
    }

    public static function emit(string $kind, mixed $payload, array $meta = [], ?string $fixedId = null): ?string
    {
        try {
            self::boot();
            if (!self::$enabled) {
                return null;
            }
            $id = $fixedId ?? self::ulid();
            $envelope = [
                'id' => $id,
                'ts' => self::timestamp(),
                'run' => self::$run ??= 'php-' . self::ulid(),
                'kind' => $kind,
                'payload' => $payload,
                'origin' => self::origin(),
            ];
            $parent = self::$scopes === [] ? null : self::$scopes[count(self::$scopes) - 1];
            if ($parent !== null && $parent !== $id) {
                $envelope['parent'] = $parent;
            }
            if ($meta !== []) {
                $envelope['meta'] = $meta;
            }
            if (self::$project !== null) {
                $envelope['project'] = self::$project;
            }

            if (count(self::$queue) >= self::MAX_QUEUE) {
                array_shift(self::$queue);
                self::$dropped++;
            }
            self::$queue[] = $envelope;

            if (!self::$shutdownRegistered) {
                self::$shutdownRegistered = true;
                register_shutdown_function([self::class, 'shutdown']);
            }
            if (count(self::$queue) >= self::BATCH || microtime(true) - self::$lastFlush >= self::FLUSH_INTERVAL) {
                self::flush(2.0);
            }
            return $id;
        } catch (Throwable) {
            return null;
        }
    }

    /** Sends everything queued, in batches of BATCH, within roughly $timeout seconds. */
    public static function flush(float $timeout = 2.0): void
    {
        try {
            self::$lastFlush = microtime(true);
            $deadline = self::$lastFlush + max(0.0, $timeout);
            while (self::$queue !== []) {
                $batch = array_splice(self::$queue, 0, self::BATCH);
                $now = microtime(true);
                if ($now < self::$downUntil || $now >= $deadline) {
                    self::$dropped += count($batch);
                    continue;
                }
                $reported = self::$dropped;
                if ($reported > 0) {
                    $batch[0]['meta'] = array_merge($batch[0]['meta'] ?? [], ['dropped' => $reported]);
                    self::$dropped = 0;
                }
                $json = json_encode($batch, self::JSON_FLAGS);
                if ($json === false) {
                    self::$dropped += $reported + count($batch);
                    continue;
                }
                $budget = (int) (($deadline - $now) * 1000);
                if (!self::post($json, min(self::TIMEOUT_MS, $budget))) {
                    self::$downUntil = microtime(true) + self::RETRY_AFTER;
                    self::$dropped += $reported + count($batch);
                    self::warnOnce();
                }
            }
        } catch (Throwable) {
            // Never let delivery break the host program.
        }
    }

    /** One line on stderr for CLI scripts; web requests stay silent rather than flood error logs. */
    private static function warnOnce(): void
    {
        if (self::$warned || PHP_SAPI !== 'cli' || !defined('STDERR')) {
            return;
        }
        self::$warned = true;
        @fwrite(STDERR, 'glint: cannot reach ' . self::$url . "; events are dropped until it answers\n");
    }

    public static function shutdown(): void
    {
        try {
            self::flush(2.0);
        } catch (Throwable) {
            // Shutdown must stay quiet.
        }
    }

    private static function enabledFromEnv(): bool
    {
        $v = strtolower(trim((string) self::env('GLINT_ENABLED')));
        if (in_array($v, ['0', 'false', 'off'], true)) {
            return false;
        }
        if (in_array($v, ['1', 'true', 'on'], true)) {
            return true;
        }
        return trim((string) self::env('APP_ENV')) !== 'production';
    }

    /** One POST of a JSON array to /v1/events. False when Glint is unreachable or answers 5xx. */
    private static function post(string $json, int $timeoutMs): bool
    {
        $url = rtrim(self::$url, '/') . '/v1/events';
        $timeoutMs = max(1, $timeoutMs);

        if (function_exists('curl_init')) {
            $ch = curl_init($url);
            if ($ch === false) {
                return false;
            }
            curl_setopt_array($ch, [
                CURLOPT_POST => true,
                CURLOPT_POSTFIELDS => $json,
                CURLOPT_HTTPHEADER => ['Content-Type: application/json'],
                CURLOPT_RETURNTRANSFER => true,
                CURLOPT_CONNECTTIMEOUT_MS => min(self::CONNECT_MS, $timeoutMs),
                CURLOPT_TIMEOUT_MS => $timeoutMs,
            ]);
            $body = curl_exec($ch);
            $errno = curl_errno($ch);
            $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
            curl_close($ch);
            return $body !== false && $errno === 0 && $status < 500;
        }

        $ctx = stream_context_create([
            'http' => [
                'method' => 'POST',
                'header' => "Content-Type: application/json\r\n",
                'content' => $json,
                'timeout' => $timeoutMs / 1000,
                'ignore_errors' => true,
            ],
        ]);
        $body = @file_get_contents($url, false, $ctx);
        if ($body === false) {
            return false;
        }
        $status = 0;
        if (isset($http_response_header[0]) && preg_match('#\s(\d{3})\s#', $http_response_header[0], $m)) {
            $status = (int) $m[1];
        }
        return $status > 0 && $status < 500;
    }

    /** origin block; file/line/fn come from the first stack frame outside this SDK's src directory. */
    private static function origin(): array
    {
        $o = [
            'lang' => 'php',
            'sdk' => self::SDK,
            'runtime' => PHP_VERSION,
            'pid' => (int) getmypid(),
        ];
        $host = gethostname();
        if ($host !== false) {
            $o['host'] = $host;
        }
        $cwd = getcwd();
        if ($cwd !== false) {
            $o['cwd'] = $cwd;
        }
        if (self::$at !== null) { // given by emitAt(); [] means none
            return $o + array_filter(self::$at, static fn ($v) => $v !== null);
        }

        // Frame k: 'file'/'line' is where frame k's function was called; 'function' is that callee.
        // So the user's call site is the first frame outside src/, and its caller is frame k+1.
        $bt = debug_backtrace(DEBUG_BACKTRACE_IGNORE_ARGS, 10);
        // The call of the global glint() helper is the user's line, wherever the helper lives
        // (this package, or the copy `glint install` prepends to every script).
        $start = null;
        foreach ($bt as $i => $frame) {
            if (($frame['function'] ?? '') === 'glint' && !isset($frame['class'])) {
                $start = $i;
                break;
            }
        }
        foreach ($bt as $i => $frame) {
            $file = $frame['file'] ?? null;
            if (!is_string($file) || ($start !== null && $i < $start)) {
                continue;
            }
            if ($i !== $start && str_starts_with($file, self::SRC_DIR)) {
                continue;
            }
            $o['file'] = $file;
            $o['line'] = (int) ($frame['line'] ?? 0);
            $caller = $bt[$i + 1] ?? null;
            if (is_array($caller) && isset($caller['function'])) {
                $prefix = isset($caller['class']) ? $caller['class'] . ($caller['type'] ?? '::') : '';
                $o['fn'] = $prefix . $caller['function'];
            }
            break;
        }
        return $o;
    }

    /** ULID: 48-bit millisecond time + 80 random bits, Crockford base32. */
    private static function ulid(): string
    {
        $ms = (int) floor(microtime(true) * 1000);
        $time = '';
        for ($i = 0; $i < 10; $i++) {
            $time = self::B32[$ms % 32] . $time;
            $ms = intdiv($ms, 32);
        }
        $rand = '';
        foreach (str_split(random_bytes(16)) as $byte) {
            $rand .= self::B32[ord($byte) % 32];
        }
        return $time . $rand;
    }

    private static function timestamp(): string
    {
        $t = microtime(true);
        $sec = (int) $t;
        $us = min(999999, (int) round(($t - $sec) * 1000000));
        return gmdate('Y-m-d\TH:i:s', $sec) . sprintf('.%06dZ', $us);
    }
}
