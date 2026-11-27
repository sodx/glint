<?php

declare(strict_types=1);

namespace Glint;

/**
 * Discovered by Laravel when a project requires glint/glint with Composer. Not needed with
 * `glint install`, which hooks in on its own; useful when the configuration is cached.
 */
class LaravelServiceProvider extends \Illuminate\Support\ServiceProvider
{
    public function register(): void
    {
        Laravel::attach($this->app);
    }
}
