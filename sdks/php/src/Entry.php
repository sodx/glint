<?php

namespace Glint;

/**
 * Handle for events just sent. Its methods change how they are shown, by sending core.update
 * events that target each id. Every method returns $this, so calls chain.
 */
final class Entry
{
    private const COLORS = ['gray', 'green', 'orange', 'red', 'blue', 'purple'];

    /** @var array<int, string> */
    private array $ids;

    /** @param array<int, string|null> $ids */
    public function __construct(array $ids)
    {
        $this->ids = array_values(array_filter($ids, 'is_string'));
    }

    public function label(mixed $label): self
    {
        return $this->update(['meta' => ['label' => (string) $label]]);
    }

    public function color(mixed $color): self
    {
        return in_array($color, self::COLORS, true) ? $this->update(['meta' => ['color' => $color]]) : $this;
    }

    public function gray(): self
    {
        return $this->color('gray');
    }

    public function green(): self
    {
        return $this->color('green');
    }

    public function orange(): self
    {
        return $this->color('orange');
    }

    public function red(): self
    {
        return $this->color('red');
    }

    public function blue(): self
    {
        return $this->color('blue');
    }

    public function purple(): self
    {
        return $this->color('purple');
    }

    public function small(): self
    {
        return $this->update(['meta' => ['size' => 'sm']]);
    }

    public function large(): self
    {
        return $this->update(['meta' => ['size' => 'lg']]);
    }

    public function hide(): self
    {
        return $this->update(['hidden' => true]);
    }

    public function remove(): self
    {
        return $this->update(['removed' => true]);
    }

    /** @param array<string, mixed> $change */
    private function update(array $change): self
    {
        foreach ($this->ids as $target) {
            Transport::emit('core.update', ['target' => $target] + $change);
        }
        return $this;
    }
}
