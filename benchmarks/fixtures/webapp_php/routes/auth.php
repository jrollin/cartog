<?php

namespace App\Routes;

use App\Auth\AuthService;
use App\Auth\TokenError;
use App\Database\DatabaseConnection;
use App\Events\EventDispatcher;
use App\Services\AuthenticationService;

use function App\Auth\extract_token;
use function App\Auth\refresh_token;

/**
 * Handle login requests — delegates to AuthService::login.
 *
 * @param array<string, mixed> $request
 *
 * @return array<string, mixed>
 */
function login_route(array $request): array
{
    $service = new AuthService($request['db']);
    $token = $service->login($request['email'], $request['password']);
    if ($token !== null) {
        return ['token' => $token, 'status' => 200];
    }

    return ['error' => 'Invalid credentials', 'status' => 401];
}

/**
 * Handle the full login flow — entry point of the deep call chain.
 *
 * @param array<string, mixed> $request
 *
 * @return array<string, mixed>
 */
function handle_login(array $request): array
{
    /** @var DatabaseConnection $db */
    $db = $request['db'];
    /** @var EventDispatcher $events */
    $events = $request['events'];
    $service = new AuthenticationService($db, $events);
    $result = $service->authenticate($request['email'], $request['password']);

    return ['token' => $result['token'], 'status' => 200];
}

/**
 * Handle token refresh requests.
 *
 * @param array<string, mixed> $request
 *
 * @return array<string, mixed>
 */
function refresh_route(array $request): array
{
    $token = extract_token($request);
    if ($token === null) {
        return ['error' => 'Missing token', 'status' => 401];
    }
    try {
        return ['token' => refresh_token($token), 'status' => 200];
    } catch (TokenError $e) {
        return ['error' => $e->getMessage(), 'status' => 401];
    }
}
