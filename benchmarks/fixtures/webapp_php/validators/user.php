<?php

namespace App\Validators;

use App\ValidationError;

/**
 * Validates user-facing input payloads.
 */
class UserValidator
{
    /**
     * Validate a user registration payload.
     *
     * @param array<string, mixed> $payload
     */
    public function validate(array $payload): bool
    {
        if (!isset($payload['email']) || !str_contains((string) $payload['email'], '@')) {
            throw new ValidationError('Invalid email', 'email');
        }
        if (strlen((string) ($payload['password'] ?? '')) < 8) {
            throw new ValidationError('Password too short', 'password');
        }

        return true;
    }
}
