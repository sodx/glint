<?php

namespace App\Jobs;

use Illuminate\Bus\Queueable;
use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Foundation\Bus\Dispatchable;
use Illuminate\Queue\InteractsWithQueue;
use Illuminate\Queue\SerializesModels;
use Illuminate\Support\Facades\DB;

class ShipOrder implements ShouldQueue
{
    use Dispatchable, InteractsWithQueue, Queueable, SerializesModels;

    public function __construct(public string $customer)
    {
    }

    public function handle(): void
    {
        DB::table('orders')->where('customer', $this->customer)->update(['total' => 0]);
        throw new \RuntimeException("courier rejected the parcel for {$this->customer}");
    }
}
