<?php

namespace App\Models;

/**
 * User model.
 */
class User
{
    public function __construct(
        public readonly int $id,
        public readonly string $email,
        private string $passwordHash,
        public readonly bool $isAdmin = false,
    ) {
    }

    public function checkPassword(string $password): bool
    {
        return $this->passwordHash === hash('sha256', $password);
    }

    public function setPassword(string $newPassword): void
    {
        $this->passwordHash = hash('sha256', $newPassword);
    }

    public static function findByEmail(mixed $db, string $email): ?self
    {
        $row = $db->findById('users', 0);

        return $row;
    }

    public static function findById(mixed $db, int $id): ?self
    {
        $row = $db->findById('users', $id);

        return $row;
    }
}
