@extends('layouts.app')

@section('title', 'Users')

@section('content')
<div class="container">
    <h1>All Users</h1>

    @if($users->isEmpty())
        <p>No users found.</p>
    @else
        <ul class="user-list">
            @foreach($users as $user)
                <li class="user-item">
                    <strong>{{ $user->name }}</strong>
                    <span>{{ $user->email }}</span>
                    <a href="{{ route('users.show', $user) }}">View</a>
                </li>
            @endforeach
        </ul>
    @endif
</div>
@endsection
