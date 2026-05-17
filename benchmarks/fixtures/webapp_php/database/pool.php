<?php

namespace App\Database;

use App\DatabaseError;
use App\Utils\Logger;

use function App\Utils\get_logger;

/**
 * A single pooled database connection handle.
 */
class ConnectionHandle
{
    public bool $inUse = false;
    public int $queryCount = 0;

    public function __construct(public readonly string $id)
    {
    }
}

/**
 * Manages a pool of database connections.
 */
class ConnectionPool
{
    private Logger $logger;

    /** @var list<ConnectionHandle> */
    private array $connections = [];

    private bool $initialized = false;

    public function __construct(private string $dsn, private int $poolSize = 10)
    {
        $this->logger = get_logger('database.pool');
        $this->logger->info("Pool created: size={$poolSize}");
    }

    public function doInitialize(): void
    {
        if ($this->initialized) {
            return;
        }
        for ($i = 0; $i < $this->poolSize; $i++) {
            $this->connections[] = new ConnectionHandle("conn-{$i}");
        }
        $this->initialized = true;
        $this->logger->info("Pool initialized with {$this->poolSize} connections");
    }

    /**
     * Acquire a connection from the pool.
     */
    public function getConnection(): ConnectionHandle
    {
        if (!$this->initialized) {
            $this->doInitialize();
        }
        foreach ($this->connections as $conn) {
            if (!$conn->inUse) {
                $conn->inUse = true;
                $conn->queryCount++;
                $this->logger->info("Acquired connection {$conn->id}");

                return $conn;
            }
        }
        throw new DatabaseError('Connection pool exhausted');
    }

    public function releaseConnection(ConnectionHandle $handle): void
    {
        $handle->inUse = false;
        $this->logger->info("Released connection {$handle->id}");
    }
}
