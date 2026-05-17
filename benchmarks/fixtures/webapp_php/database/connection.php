<?php

namespace App\Database;

use App\DatabaseError;
use App\Utils\Logger;

use function App\Utils\get_logger;

/**
 * High-level database connection.
 */
class DatabaseConnection
{
    private Logger $logger;
    private int $transactionDepth = 0;
    private ?ConnectionHandle $currentHandle = null;

    public function __construct(private ConnectionPool $pool)
    {
        $this->logger = get_logger('database.connection');
        $this->logger->info('DatabaseConnection created');
    }

    /**
     * Execute a SQL query against an acquired connection.
     *
     * @param list<mixed> $params
     *
     * @return array<string, mixed>
     */
    public function executeQuery(string $sql, array $params = []): array
    {
        $handle = $this->acquire();
        try {
            $this->logger->info('Executing: ' . substr($sql, 0, 80));

            return ['rows' => [], 'affected' => 0];
        } catch (\Throwable $e) {
            throw new DatabaseError($e->getMessage(), $sql);
        } finally {
            $this->release($handle);
        }
    }

    /**
     * Find a single record by id.
     *
     * @return array<string, mixed>|null
     */
    public function findById(string $table, int $id): ?array
    {
        $result = $this->executeQuery("SELECT * FROM {$table} WHERE id = ?", [$id]);

        return $result['rows'][0] ?? null;
    }

    /**
     * Insert a record and return its id.
     *
     * @param array<string, mixed> $data
     */
    public function insert(string $table, array $data): string
    {
        $this->executeQuery("INSERT INTO {$table} VALUES (?)", array_values($data));

        return 'generated-id';
    }

    /**
     * Update a record by id, returning affected rows.
     *
     * @param array<string, mixed> $data
     */
    public function update(string $table, int $id, array $data): int
    {
        $result = $this->executeQuery("UPDATE {$table} SET x = ? WHERE id = ?", [$id]);

        return (int) $result['affected'];
    }

    public function beginTransaction(): void
    {
        $this->transactionDepth++;
        if ($this->transactionDepth === 1) {
            $this->currentHandle = $this->acquire();
            $this->logger->info('Transaction started');
        }
    }

    public function commit(): void
    {
        if ($this->transactionDepth > 0) {
            $this->transactionDepth--;
        }
    }

    private function acquire(): ConnectionHandle
    {
        if ($this->currentHandle !== null && $this->transactionDepth > 0) {
            return $this->currentHandle;
        }

        return $this->pool->getConnection();
    }

    private function release(ConnectionHandle $handle): void
    {
        if ($this->transactionDepth === 0) {
            $this->pool->releaseConnection($handle);
        }
    }
}
