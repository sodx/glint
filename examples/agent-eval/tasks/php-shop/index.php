<?php
// GET /?order=<id> → {"order": id, "customer": name, "total": n}
$orders = require __DIR__ . '/orders.php';
$id = (int) ($_GET['order'] ?? 0);
if (!isset($orders[$id])) {
    http_response_code(404);
    exit;
}
$order = $orders[$id];

function discount(array $customer): float
{
    return $customer['tier'] === 'gold' ? 0.1 : 0.0;
}

$subtotal = 0;
foreach ($order['lines'] as $line) {
    $subtotal += $line['price'] * $line['qty'];
}
$total = round($subtotal * (1 - discount($order['customer'])), 2);
header('Content-Type: application/json');
echo json_encode(['order' => $id, 'customer' => $order['customer']['name'], 'total' => $total]);
