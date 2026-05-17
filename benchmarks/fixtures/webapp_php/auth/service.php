<?php

namespace App\Auth;

use App\Database\DatabaseConnection;
use App\Models\User;
use App\Utils\Logger;

use function App\Utils\get_logger;

/**
 * Base service with common utilities.
 */
class BaseService
{
    protected Logger $logger;

    public function __construct()
    {
        $this->logger = get_logger(static::class);
    }

    protected function log(string $message): void
    {
        $this->logger->info('[' . static::class . "] {$message}");
    }
}

/**
 * Handles user authentication flows.
 */
class AuthService extends BaseService
{
    public function __construct(private DatabaseConnection $db)
    {
        parent::__construct();
    }

    /**
     * Authenticate a user with email + password, returning a token on success.
     */
    public function login(string $email, string $password): ?string
    {
        $user = $this->findUser($email);
        if ($user !== null && $user->checkPassword($password)) {
            $this->log("Login successful for {$email}");

            return generate_token($user);
        }
        $this->log("Login failed for {$email}");

        return null;
    }

    public function logout(string $token): void
    {
        revoke_token($token);
    }

    public function getCurrentUser(string $token): User
    {
        return validate_token($token);
    }

    public function changePassword(string $token, string $oldPw, string $newPw): bool
    {
        $user = validate_token($token);
        if ($user->checkPassword($oldPw)) {
            $user->setPassword($newPw);

            return true;
        }

        return false;
    }

    private function findUser(string $email): ?User
    {
        return User::findByEmail($this->db, $email);
    }
}

/**
 * Extended auth service for admin operations.
 */
class AdminService extends AuthService
{
    public function impersonate(string $adminToken, int $userId): string
    {
        $admin = $this->getCurrentUser($adminToken);
        if ($admin->isAdmin) {
            $this->log("Admin {$admin->email} impersonating user {$userId}");

            return generate_token($admin);
        }
        throw new TokenError('Not authorized');
    }

    /**
     * @return list<User>
     */
    public function listAllUsers(string $adminToken): array
    {
        $admin = $this->getCurrentUser($adminToken);
        if ($admin->isAdmin) {
            return [];
        }
        throw new TokenError('Not authorized');
    }
}
