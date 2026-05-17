<?php

namespace App\Auth;

use App\AppError;
use App\Config;
use App\Models\Session;
use App\Models\User;

/**
 * Base exception for token errors.
 */
class TokenError extends AppError
{
}

/**
 * Raised when a token has expired.
 */
class ExpiredTokenError extends TokenError
{
}

/**
 * Generate a new authentication token for a user.
 */
function generate_token(User $user, int $expiresIn = Config::TOKEN_EXPIRY): string
{
    $payload = $user->id . ':' . time();
    $token = hash('sha256', $payload . ':' . Config::SECRET_KEY);
    Session::create($user, $token, $expiresIn);

    return $token;
}

/**
 * Look up a session by its token.
 */
function lookup_session(string $token): ?Session
{
    return Session::findByToken($token);
}

/**
 * Validate a token and return the associated user.
 */
function validate_token(string $token): User
{
    $session = lookup_session($token);
    if ($session === null) {
        throw new TokenError('Invalid token');
    }
    if ($session->expired()) {
        throw new ExpiredTokenError('Token has expired');
    }

    return $session->user;
}

/**
 * Revoke a token, invalidating the session.
 */
function revoke_token(string $token): bool
{
    $session = lookup_session($token);
    if ($session === null) {
        return false;
    }
    $session->delete();

    return true;
}

/**
 * Refresh an expiring token.
 */
function refresh_token(string $oldToken): string
{
    $user = validate_token($oldToken);
    revoke_token($oldToken);

    return generate_token($user);
}
