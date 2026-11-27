<?php

// Loaded by Glint\Laravel::preload() in a Laravel app only, before Laravel's own helpers. Laravel
// defines env() only when none exists yet and calls it while the configuration loads: that first
// call is when Glint hooks into the application. It returns exactly what Laravel's env() does.

if (!function_exists('env')) {
    function env($key, $default = null)
    {
        \Glint\Laravel::fromEnv();
        if (class_exists(\Illuminate\Support\Env::class)) {
            return \Illuminate\Support\Env::get($key, $default);
        }
        $value = getenv($key);
        return $value === false ? ($default instanceof \Closure ? $default() : $default) : $value;
    }
}
