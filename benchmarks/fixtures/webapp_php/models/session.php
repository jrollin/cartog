<?php

namespace App\Models;

/**
 * Session model.
 */
class Session
{
    public function __construct(
        public readonly User $user,
        public readonly string $token,
        public readonly int $expiresAt,
    ) {
    }

    public function expired(): bool
    {
        return time() > $this->expiresAt;
    }

    public function delete(): void
    {
        // Remove session from storage.
    }

    public static function create(User $user, string $token, int $expiresIn): self
    {
        return new self($user, $token, time() + $expiresIn);
    }

    public static function findByToken(string $token): ?self
    {
        return null;
    }
}
