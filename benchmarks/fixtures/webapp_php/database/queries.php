<?php

namespace App\Database;

use function App\Utils\get_logger;

/**
 * User-related query helpers.
 */
class UserQueries
{
    public function __construct(private DatabaseConnection $db)
    {
    }

    /**
     * @return array<string, mixed>|null
     */
    public function findByEmail(string $email): ?array
    {
        get_logger('database.queries')->info("Finding user by email: {$email}");
        $result = $this->db->executeQuery('SELECT * FROM users WHERE email = ?', [$email]);

        return $result['rows'][0] ?? null;
    }

    public function softDelete(int $userId): bool
    {
        get_logger('database.queries')->info("Soft-deleting user {$userId}");

        return $this->db->update('users', $userId, ['deleted_at' => time()]) > 0;
    }
}

/**
 * Session-related query helpers.
 */
class SessionQueries
{
    public function __construct(private DatabaseConnection $db)
    {
    }

    /**
     * @return array<string, mixed>|null
     */
    public function findActiveSession(string $token): ?array
    {
        $result = $this->db->executeQuery('SELECT * FROM sessions WHERE token_hash = ?', [$token]);

        return $result['rows'][0] ?? null;
    }

    public function expireSession(int $sessionId): bool
    {
        return $this->db->update('sessions', $sessionId, ['expired_at' => time()]) > 0;
    }
}

/**
 * Payment-related query helpers.
 */
class PaymentQueries
{
    public function __construct(private DatabaseConnection $db)
    {
    }

    /**
     * @return array<string, mixed>|null
     */
    public function findByTransactionId(string $txnId): ?array
    {
        $result = $this->db->executeQuery('SELECT * FROM payments WHERE transaction_id = ?', [$txnId]);

        return $result['rows'][0] ?? null;
    }

    public function createPayment(int $userId, float $amount, string $currency, string $txnId): string
    {
        get_logger('database.queries')->info("Creating payment {$txnId}");

        return $this->db->insert('payments', [
            'user_id' => $userId,
            'amount' => $amount,
            'currency' => $currency,
            'transaction_id' => $txnId,
        ]);
    }

    public function updateStatus(string $txnId, string $status): bool
    {
        get_logger('database.queries')->info("Updating payment {$txnId} to {$status}");
        $result = $this->db->executeQuery('UPDATE payments SET status = ? WHERE transaction_id = ?', [$status, $txnId]);

        return ($result['affected'] ?? 0) > 0;
    }
}
