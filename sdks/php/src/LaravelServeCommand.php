<?php

declare(strict_types=1);

namespace Glint;

/**
 * `php artisan serve`, with Glint: PHP runs no auto_prepend_file for a built-in server router
 * script, so the server gets laravel-server.php as its router, which loads the prepend file and
 * then Laravel's own router. Laravel::attach() puts this command in place of the original.
 */
class LaravelServeCommand extends \Illuminate\Foundation\Console\ServeCommand
{
    protected function serverCommand()
    {
        $command = parent::serverCommand();
        foreach ($command as $i => $part) {
            if (is_string($part) && str_ends_with($part, 'server.php') && is_file($part)) {
                $command[$i] = __DIR__ . '/laravel-server.php';
            }
        }
        return $command;
    }
}
