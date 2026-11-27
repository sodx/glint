<?php

namespace Glint;

use Throwable;

/**
 * Static facade. Contract: spec/conformance/README.md. Never throws into the host program
 * (except for what your own callable throws in measure()).
 *
 *     Glint::log('user signed in', ['user_id' => 7], 'warn');
 *     Glint::table($rows)->label('orders');
 */
final class Glint
{
    /** Sends each value as its own event; a Throwable becomes core.exception. Used by glint(). */
    public static function values(mixed ...$values): Entry
    {
        $ids = [];
        foreach ($values as $value) {
            try {
                if ($value instanceof Throwable) {
                    $ids[] = Transport::emit('core.exception', self::exceptionPayload($value));
                    continue;
                }
                $payload = ['value' => Encoder::encode($value)];
                $type = Encoder::typeName($value);
                if ($type !== null) {
                    $payload['type'] = $type;
                }
                $ids[] = Transport::emit('core.value', $payload);
            } catch (Throwable) {
                $ids[] = null;
            }
        }
        return new Entry($ids);
    }

    /** @param array<string, mixed> $fields */
    public static function log(mixed $message, array $fields = [], string $level = 'info'): Entry
    {
        try {
            $payload = ['message' => (string) $message];
            if ($fields !== []) {
                $payload['fields'] = Encoder::encode($fields);
            }
            return new Entry([Transport::emit('core.log', $payload, ['level' => $level])]);
        } catch (Throwable) {
            return new Entry([]);
        }
    }

    public static function table(iterable $rows, ?array $columns = null): Entry
    {
        try {
            $list = is_array($rows) ? array_values($rows) : iterator_to_array($rows, false);
            $payload = ['rows' => Encoder::encode($list)];
            if ($columns !== null) {
                $payload['columns'] = array_map('strval', $columns);
            }
            return new Entry([Transport::emit('core.table', $payload)]);
        } catch (Throwable) {
            return new Entry([]);
        }
    }

    public static function exception(Throwable $e): Entry
    {
        try {
            return new Entry([Transport::emit('core.exception', self::exceptionPayload($e))]);
        } catch (Throwable) {
            return new Entry([]);
        }
    }

    /**
     * Runs $fn, sends core.measure with its wall time, and returns its result. Exceptions from $fn
     * still reach the caller; the measurement is sent first.
     */
    public static function measure(string $name, callable $fn): mixed
    {
        $start = hrtime(true);
        try {
            return $fn();
        } finally {
            try {
                Transport::emit('core.measure', [
                    'name' => $name,
                    'duration_ms' => round((hrtime(true) - $start) / 1e6, 3),
                ]);
            } catch (Throwable) {
                // Measuring must not change what the caller sees.
            }
        }
    }

    /** Any kind, sent unchanged (the payload and meta are still encoded). */
    public static function send(string $kind, mixed $payload, array $meta = []): Entry
    {
        try {
            $p = Encoder::encode($payload);
            if ($p === []) {
                $p = new \stdClass();
            }
            return new Entry([Transport::emit($kind, $p, $meta === [] ? [] : Encoder::encode($meta))]);
        } catch (Throwable) {
            return new Entry([]);
        }
    }

    /** Sends queued events now, blocking for at most about $timeout seconds. */
    public static function flush(float $timeout = 2.0): void
    {
        Transport::flush($timeout);
    }

    /**
     * Overrides the environment. Null leaves a setting as it is.
     * $enabled = false turns the SDK off; $url is the Glint server base URL.
     */
    public static function configure(?string $url = null, ?bool $enabled = null, ?string $project = null): void
    {
        try {
            Transport::configure($url, $enabled, $project);
        } catch (Throwable) {
            // Configuration problems must not reach the host program.
        }
    }

    /** @return array<string, mixed> */
    private static function exceptionPayload(Throwable $e, int $depth = 0): array
    {
        $trace = $e->getTrace();

        // Locations: where the throw happened, then where each traced call was made.
        // Enclosing function of each location: the thrower, then each caller in turn.
        $locations = [[$e->getFile(), $e->getLine()]];
        $enclosing = [];
        foreach ($trace as $t) {
            $locations[] = [$t['file'] ?? null, $t['line'] ?? null];
            $enclosing[] = [$t['function'] ?? null, $t['class'] ?? null];
        }
        $enclosing[] = [null, null];

        $frames = [];
        foreach ($locations as $k => [$file, $line]) {
            [$fn, $class] = $enclosing[$k];
            $frame = [
                'vendor' => is_string($file) && str_contains($file, '/vendor/'),
            ];
            if (is_string($file)) {
                $frame['file'] = $file;
            }
            if (is_int($line)) {
                $frame['line'] = $line;
            }
            if (is_string($fn)) {
                $frame['fn'] = $fn;
            }
            if (is_string($class)) {
                $frame['class'] = $class;
            }
            $frames[] = $frame;
        }

        $payload = [
            'class' => get_class($e),
            'message' => $e->getMessage(),
            'frames' => $frames,
        ];
        $previous = $e->getPrevious();
        if ($previous !== null && $depth < 5) {
            $payload['cause'] = self::exceptionPayload($previous, $depth + 1);
        }
        return $payload;
    }
}
