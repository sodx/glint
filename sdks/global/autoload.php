<?php
// Written by `glint install`: glint() in every PHP script on this machine, and uncaught errors
// reported to Glint without any code. Off in production like the SDK (APP_ENV=production or
// GLINT_ENABLED=0).
//
// Lazy on purpose: nothing is loaded until it is used, and a project that requires glint/glint
// itself gets its own version (Composer registers its autoloader first).
spl_autoload_register(static function (string $class): void {
    if (strncmp($class, 'Glint\\', 6) === 0) {
        $file = __DIR__ . '/src/' . substr($class, 6) . '.php';
        if (is_file($file)) {
            require_once $file;
        }
    }
}, true, false);
if (!function_exists('glint')) {
    require_once __DIR__ . '/src/helpers.php';
}

// Frameworks, with no plugin or package: when this script belongs to a WordPress site or a Laravel
// app, Glint hooks in before it loads (see src/WordPress.php, src/Laravel.php). Other scripts only
// pay for a few file checks.
(static function (): void {
    $script = (string) ($_SERVER['SCRIPT_FILENAME'] ?? '');
    $dir = dirname($script !== '' && $script[0] !== '/' ? (string) realpath($script) : $script);
    for ($i = 0; $i < 3 && strlen($dir) > 1; $i++, $dir = dirname($dir)) {
        if (is_file($dir . '/wp-settings.php')) {
            try {
                \Glint\WordPress::preload();
            } catch (\Throwable) {
            }
            return;
        }
        if (is_file($dir . '/artisan') && is_dir($dir . '/vendor/laravel/framework')) {
            try {
                \Glint\Laravel::preload($dir);
            } catch (\Throwable) {
            }
            return;
        }
    }
    // `php artisan serve` runs PHP's built-in server in public/ with a router script from vendor/.
    if (PHP_SAPI === 'cli-server') {
        $dir = dirname((string) getcwd());
        if (is_file($dir . '/artisan') && is_dir($dir . '/vendor/laravel/framework')) {
            try {
                \Glint\Laravel::preload($dir);
            } catch (\Throwable) {
            }
        }
    }
})();

// Uncaught exceptions: report, then rethrow, so PHP prints its usual "Uncaught …" error and exits
// as it would have. An application that installs its own handler (a framework) replaces this one.
set_exception_handler(static function (Throwable $e): void {
    try {
        \Glint\Glint::exception($e)->label('uncaught');
    } catch (Throwable) {
        // a debugging aid never changes what the program does
    }
    restore_exception_handler();
    throw $e;
});

// Fatal errors that are not exceptions (out of memory, a parse error in an included file, …).
register_shutdown_function(static function (): void {
    $err = error_get_last();
    if ($err === null || !($err['type'] & (E_ERROR | E_PARSE | E_CORE_ERROR | E_COMPILE_ERROR))) {
        return;
    }
    if (str_starts_with($err['message'], 'Uncaught ')) {
        return; // reported by the exception handler above
    }
    try {
        \Glint\Glint::send('core.exception', [
            'class' => 'FatalError',
            'message' => $err['message'],
            'frames' => [['file' => $err['file'], 'line' => $err['line']]],
        ], ['level' => 'fatal', 'label' => 'uncaught']);
    } catch (Throwable) {
    }
});
