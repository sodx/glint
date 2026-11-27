<?php

namespace Glint;

use BackedEnum;
use Closure;
use DateTimeInterface;
use JsonSerializable;
use stdClass;
use Throwable;
use UnitEnum;

/**
 * Turns PHP values into JSON-ready data that says what the values were.
 * Rules: spec/conformance/README.md, requirement 4.
 */
final class Encoder
{
    public const MAX_DEPTH = 10;
    private const MAX_ITEMS = 1000;
    private const MAX_STRING = 100000;

    /** Plain data for json_encode(). Objects come back as stdClass with their fields. */
    public static function encode(mixed $value): mixed
    {
        return self::walk($value, 0, '$', []);
    }

    /** Source type name for a top-level value (objects only), or null. */
    public static function typeName(mixed $value): ?string
    {
        return is_object($value) ? get_class($value) : null;
    }

    /**
     * @param array<int, string> $ancestors spl_object_id => JSON path, along the current branch only
     */
    private static function walk(mixed $v, int $depth, string $path, array $ancestors): mixed
    {
        if ($v === null || is_bool($v) || is_int($v)) {
            return $v;
        }
        if (is_float($v)) {
            if (is_nan($v)) {
                return 'NaN';
            }
            if (is_infinite($v)) {
                return $v > 0 ? 'Infinity' : '-Infinity';
            }
            return $v;
        }
        if (is_string($v)) {
            return self::walkString($v);
        }
        if (is_array($v)) {
            return self::walkArray($v, $depth, $path, $ancestors);
        }
        if (is_object($v)) {
            return self::walkObject($v, $depth, $path, $ancestors);
        }
        if (is_resource($v)) {
            return 'resource(' . get_resource_type($v) . ')';
        }
        return gettype($v);
    }

    private static function walkString(string $s): mixed
    {
        if (preg_match('//u', $s) !== 1) {
            return ['$bin' => base64_encode(substr($s, 0, self::MAX_STRING))];
        }
        if (strlen($s) > self::MAX_STRING) {
            $s = substr($s, 0, self::MAX_STRING);
            while ($s !== '' && preg_match('//u', $s) !== 1) {
                $s = substr($s, 0, -1); // do not leave half a character behind
            }
            $s .= '…';
        }
        return $s;
    }

    private static function walkArray(array $a, int $depth, string $path, array $ancestors): array
    {
        if ($depth >= self::MAX_DEPTH) {
            return ['$truncated' => true];
        }
        $list = array_is_list($a);
        $total = count($a);
        $out = [];
        $n = 0;
        foreach ($a as $k => $x) {
            if ($n === self::MAX_ITEMS) {
                if ($list) {
                    $out[] = ['$truncated' => true, 'more' => $total - self::MAX_ITEMS];
                } else {
                    $out['$truncated'] = true;
                }
                break;
            }
            if ($list) {
                $out[] = self::walk($x, $depth + 1, "{$path}[{$k}]", $ancestors);
            } else {
                $out[$k] = self::walk($x, $depth + 1, "{$path}.{$k}", $ancestors);
            }
            $n++;
        }
        return $out; // list -> JSON array; keyed -> JSON object (json_encode decides)
    }

    private static function walkObject(object $v, int $depth, string $path, array $ancestors): mixed
    {
        $id = spl_object_id($v);
        if (isset($ancestors[$id])) {
            return ['$ref' => $ancestors[$id]];
        }
        if ($depth >= self::MAX_DEPTH) {
            return ['$truncated' => true];
        }
        if ($v instanceof Closure) {
            return 'Closure';
        }
        if ($v instanceof DateTimeInterface) {
            return $v->format(DATE_ATOM);
        }
        if ($v instanceof BackedEnum) {
            return self::walk($v->value, $depth, $path, $ancestors);
        }
        if ($v instanceof UnitEnum) {
            return $v->name;
        }

        $ancestors[$id] = $path;
        try {
            if ($v instanceof JsonSerializable) {
                return self::walk($v->jsonSerialize(), $depth + 1, $path, $ancestors);
            }
            $out = [];
            $n = 0;
            foreach ((array) $v as $k => $x) {
                if ($n === self::MAX_ITEMS) {
                    $out['$truncated'] = true;
                    break;
                }
                // Private and protected properties come out of (array) with "\0Class\0" or "\0*\0" prefixes.
                $key = (string) $k;
                if ($key !== '' && $key[0] === "\0") {
                    $key = substr($key, strrpos($key, "\0") + 1);
                }
                $out[$key] = self::walk($x, $depth + 1, "{$path}.{$key}", $ancestors);
                $n++;
            }
            return (object) $out;
        } catch (Throwable $e) {
            return '[threw: ' . $e->getMessage() . ']';
        }
    }
}
