<?php

declare(strict_types=1);

namespace Glint;

/** Thrown by Glint::pause() when someone answers the pause with Stop. */
final class GlintStop extends \RuntimeException
{
}
