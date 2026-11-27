<?php
// A small PHP shop served by `php -S`, debugged with the global glint() that `glint install`
// provides: no require, no composer. Run with examples/e2e/run.py, or:
//   php -S 127.0.0.1:8091 -t examples/e2e/shop     then open /cart/7, /checkout/7, /checkout/8
//
// Served from a document root on purpose: PHP never applies auto_prepend_file to a `php -S`
// router script (`php -S host:port router.php`, which is also what `artisan serve` runs).

use Glint\Glint;

$db = new PDO('sqlite::memory:');
$db->setAttribute(PDO::ATTR_ERRMODE, PDO::ERRMODE_EXCEPTION);
$db->exec('create table orders (id integer primary key, customer text, status text, total real)');
$db->exec('create table order_items (order_id integer, sku text, qty integer, price real)');
$db->exec("insert into orders values (7, 'Ada Lovelace', 'open', null), (8, 'Alan Turing', 'open', null)");
$db->exec("insert into order_items values (7, 'A-1', 2, 10.5), (7, 'B-7', 1, 99), (8, 'C-3', 1, 15)");

/** Runs a query and shows it in Glint with its bindings and timing, like a framework's query log. */
function query(PDO $db, string $sql, array $bindings = []): array
{
    $t = microtime(true);
    $st = $db->prepare($sql);
    $st->execute($bindings);
    $rows = $st->fetchAll(PDO::FETCH_ASSOC);
    Glint::send('sql.query', [
        'sql' => $sql,
        'bindings' => $bindings,
        'duration_ms' => round((microtime(true) - $t) * 1000, 2),
        'conn' => 'sqlite',
        'rows' => count($rows),
    ]);
    return $rows;
}

set_exception_handler(static function (Throwable $e): void {
    glint($e);
    http_response_code(500);
    echo "500: {$e->getMessage()}\n";
});

$path = parse_url($_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH);
Glint::log("{$_SERVER['REQUEST_METHOD']} {$path}", ['ip' => $_SERVER['REMOTE_ADDR'] ?? null]);

if (preg_match('#^/cart/(\d+)$#', $path, $m)) {
    $items = query($db, 'select sku, qty, price from order_items where order_id = ?', [(int) $m[1]]);
    Glint::table($items)->label('cart');
    echo json_encode($items), "\n";
    return;
}

if (preg_match('#^/checkout/(\d+)$#', $path, $m)) {
    // Everything checkout() sends nests under this measure: the request reads as a call tree.
    Glint::measure('checkout', static fn () => checkout($db, (int) $m[1]));
    return;
}

function checkout(PDO $db, int $id): void
{
    $order = Glint::measure('load order', static function () use ($db, $id): array {
        $order = query($db, 'select * from orders where id = ? and status in (?, ?)', [$id, 'open', 'paid'])[0] ?? [];
        $order['items'] = query($db, 'select sku, qty, price from order_items where order_id = ?', [$id]);
        return $order;
    });
    glint($order)->label('order before pricing');

    $total = Glint::measure('pricing', static function () use ($order): float {
        usleep(30_000);
        return array_sum(array_map(static fn (array $i): float => $i['qty'] * $i['price'], $order['items']));
    });
    if ($total < 20) {
        Glint::log('order below minimum, applying fee', ['order' => $id, 'total' => $total], 'warn');
        // The bug this demo is about: the fee table has no entry for small orders.
        $fees = ['standard' => 2.5];
        $total += $fees[$order['customer'] === 'Alan Turing' ? 'small' : 'standard']
            ?? throw new UnexpectedValueException("no fee configured for order {$id}");
    }
    query($db, 'update orders set total = ?, status = ? where id = ?', [$total, 'paid', $id]);
    glint(['order' => $id, 'total' => $total, 'paid' => true])->green()->label('saved');
    echo "paid {$total}\n";
}

http_response_code(404);
echo "not found\n";
