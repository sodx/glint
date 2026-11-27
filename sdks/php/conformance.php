<?php

// Conformance program for glint-php. Run from the repo root:
//   glint conformance -- php sdks/php/conformance.php
// Scenarios: spec/conformance/scenarios.json (same order). No composer install needed.

use Glint\Glint;

spl_autoload_register(static function (string $class): void {
    $prefix = 'Glint\\';
    if (strncmp($class, $prefix, strlen($prefix)) !== 0) {
        return;
    }
    $file = __DIR__ . '/src/' . str_replace('\\', '/', substr($class, strlen($prefix))) . '.php';
    if (is_file($file)) {
        require $file;
    }
});
require_once __DIR__ . '/src/helpers.php';

class ConformanceOrder
{
    public $id = 7;
    public $total = 129.5;
}

class ConformanceNode
{
    public $name = 'a';
    public $self = null;
}

function begin(string $scenario): void
{
    Glint::send('x-conformance.begin', ['scenario' => $scenario]);
}

begin('scalars');
glint(42);
glint('hello');
glint(true);
glint(null);
glint(3.5);

begin('structured');
glint(['name' => 'Ada', 'tags' => ['vip', 'eu'], 'nested' => ['n' => 1]]);

begin('object-type');
glint(new ConformanceOrder());

begin('cycle');
$a = new ConformanceNode();
$a->self = $a;
glint($a);

begin('special-numbers');
glint(['nan' => NAN, 'inf' => INF, 'ninf' => -INF]);

begin('depth');
$deep = [];
for ($i = 0; $i < 50; $i++) {
    $deep = ['child' => $deep];
}
glint($deep);

begin('multiple-values');
glint(1, 2)->label('pair');

begin('fluent');
glint('styled')->color('red')->label('L')->large();

begin('hide-remove');
glint('h')->hide();
glint('r')->remove();

begin('log');
Glint::log('user signed in', ['user_id' => 7], 'warn');

begin('table');
Glint::table([['sku' => 'A-1', 'qty' => 2], ['sku' => 'B-7', 'qty' => 1]]);

begin('exception');
function failing(): void
{
    throw new RuntimeException('boom');
}
try {
    failing();
} catch (RuntimeException $err) {
    Glint::exception($err);
}

begin('measure');
$measured = Glint::measure('work', static function (): int {
    usleep(20000);
    return 5;
});
$measureOk = $measured === 5;

begin('send');
Glint::send('x-conformance.custom', ['a' => 1], ['label' => 'raw']);

begin('origin');
glint('origin-check'); // glint:origin

// No flush here on purpose: the SDK delivers queued events from its shutdown hook.
begin('end');
glint('end');

if (!$measureOk) {
    fwrite(STDERR, "measure returned something other than 5\n");
    exit(1);
}
