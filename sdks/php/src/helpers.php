<?php

// Global helper:
//
//     glint($order)->label('before save')->orange();
//
// Defined only if the project has not defined its own glint() already.

if (!function_exists('glint')) {
    function glint(mixed ...$values): \Glint\Entry
    {
        return \Glint\Glint::values(...$values);
    }
}
