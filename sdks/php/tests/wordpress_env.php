<?php
// Regression test for the WordPress integration: only development sites report.
// Usage: php wordpress_env.php <prod|debug|local>; prints "ok" or what went wrong.
// Run by crates/glint-cli/tests/sdks.rs (wordpress_reports_only_on_development_sites).

declare(strict_types=1);

spl_autoload_register(static function (string $class): void {
    $file = __DIR__ . '/../src/' . substr($class, 6) . '.php';
    if (strncmp($class, 'Glint\\', 6) === 0 && is_file($file)) {
        require_once $file;
    }
});
require_once __DIR__ . '/../src/helpers.php';

$mode = $argv[1] ?? 'prod';
$site = sys_get_temp_dir() . '/glint-wp-' . getmypid();
@mkdir($site);
touch("$site/wp-load.php");
touch("$site/wp-settings.php");
$_SERVER['SCRIPT_FILENAME'] = "$site/index.php";
$_SERVER['REQUEST_URI'] = '/hello';
putenv('GLINT_WP_CLI=1');
putenv('GLINT_URL=http://127.0.0.1:9'); // nothing listens: held events are never sent

\Glint\WordPress::preload();
// wp-config.php runs now.
if ($mode === 'debug') {
    define('WP_DEBUG', true);
} elseif ($mode === 'local') {
    define('WP_ENVIRONMENT_TYPE', 'local');
}
$queue = new ReflectionProperty(\Glint\Transport::class, 'queue');
$queue->setAccessible(true);
$held = count($queue->getValue());
\Glint\WordPress::boot();
ob_start();
\Glint\WordPress::browserTag();
$tag = ob_get_clean();
glint('from theme code');
$after = count($queue->getValue());
$enabled = new ReflectionProperty(\Glint\Transport::class, 'enabled');
$enabled->setAccessible(true);
$on = $enabled->getValue();

@unlink("$site/wp-load.php");
@unlink("$site/wp-settings.php");
@rmdir($site);

$dev = $mode !== 'prod';
$problems = [];
if ($held < 1) {
    $problems[] = 'the request was not queued while undecided';
}
if ($dev && !str_contains($tag, '/v1/browser.js')) {
    $problems[] = 'no browser script on a development site';
}
if (!$dev && $tag !== '') {
    $problems[] = 'browser script on a production site';
}
if (!$dev && $after !== 0) {
    $problems[] = "production site kept $after events";
}
if ($dev && $on !== true) {
    $problems[] = 'development site has Glint turned off';
}
if (!$dev && $on !== false) {
    $problems[] = 'production site has Glint turned on';
}
// The queue is not sent at exit in this test.
\Glint\Transport::release(false);
echo $problems === [] ? "ok\n" : implode("; ", $problems) . "\n";
