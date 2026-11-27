<?php

// Glint's Laravel example: copied over a fresh `laravel new` app by examples/frameworks/check.py.
// Nothing here mentions Glint: the prepend file hooks in when Laravel boots.

use App\Http\Controllers\OrderController;
use Illuminate\Support\Facades\Route;

Route::get('/orders/{customer}', [OrderController::class, 'index'])->name('orders');
Route::get('/boom', [OrderController::class, 'boom']);
Route::get('/upstream', [OrderController::class, 'upstream']);
Route::get('/ship/{customer}', [OrderController::class, 'ship']);
