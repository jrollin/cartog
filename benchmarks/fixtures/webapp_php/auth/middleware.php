<?php

namespace App\Auth;

use App\AuthenticationError;
use App\Models\User;

/**
 * Extract a bearer token from a request array.
 *
 * @param array<string, mixed> $request
 */
function extract_token(array $request): ?string
{
    $header = $request['authorization'] ?? null;
    if (!is_string($header) || !str_starts_with($header, 'Bearer ')) {
        return null;
    }

    return substr($header, 7);
}

/**
 * Resolve the current user for a request, or null if unauthenticated.
 *
 * @param array<string, mixed> $request
 */
function get_current_user(array $request): ?User
{
    $token = extract_token($request);
    if ($token === null) {
        return null;
    }

    return validate_token($token);
}

/**
 * Guard a callback behind authentication.
 *
 * @param array<string, mixed> $request
 */
function auth_required(array $request, callable $handler): mixed
{
    $user = get_current_user($request);
    if ($user === null) {
        throw new AuthenticationError('Missing token');
    }
    $request['user'] = $user;

    return $handler($request);
}

/**
 * Guard a callback behind admin authentication.
 *
 * @param array<string, mixed> $request
 */
function admin_required(array $request, callable $handler): mixed
{
    return auth_required($request, function (array $req) use ($handler) {
        /** @var User $user */
        $user = $req['user'];
        if (!$user->isAdmin) {
            throw new AuthenticationError('Admin required');
        }

        return $handler($req);
    });
}
