<?php

// Router for `php artisan serve` (see LaravelServeCommand). PHP skips auto_prepend_file for a
// router script, so load it here, then hand over to the router Laravel would have used.

$glintPrepend = (string) ini_get('auto_prepend_file');
if ($glintPrepend !== '' && is_file($glintPrepend)) {
    require_once $glintPrepend;
}

$glintBase = dirname((string) getcwd());
return require (is_file($glintBase . '/server.php')
    ? $glintBase . '/server.php'
    : $glintBase . '/vendor/laravel/framework/src/Illuminate/Foundation/resources/server.php');
