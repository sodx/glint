<?php

namespace App\Http\Controllers;

use App\Jobs\ShipOrder;
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Http;
use Illuminate\Support\Facades\Log;
use Illuminate\Support\Facades\Schema;

class OrderController extends Controller
{
    public function index(string $customer)
    {
        if (!Schema::hasTable('orders')) {
            DB::statement('create table orders (id integer primary key, customer text, total real)');
        }
        DB::insert('insert into orders (customer, total) values (?, ?)', [$customer, 42.5]);
        $orders = DB::table('orders')->where('customer', $customer)->get();
        Log::warning("customer {$customer} has {$orders->count()} orders", ['customer' => $customer]);
        return ['orders' => $orders->count()];
    }

    public function boom()
    {
        $order = ['id' => 7];
        return ['email' => $order['email']]; // ErrorException: undefined array key
    }

    public function upstream()
    {
        Http::timeout(1)->get('http://127.0.0.1:59999/unreachable');
        return ['ok' => true];
    }

    public function ship(string $customer)
    {
        ShipOrder::dispatch($customer);
        return ['queued' => $customer];
    }
}
