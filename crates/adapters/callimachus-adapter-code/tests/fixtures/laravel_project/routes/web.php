<?php

use App\Http\Controllers\UserController;
use Illuminate\Support\Facades\Route;

Route::get('/', function () {
    return view('welcome');
});

Route::middleware(['auth'])->group(function () {
    Route::resource('users', UserController::class);
    Route::get('/dashboard', function () {
        return view('dashboard');
    })->name('dashboard');
});
