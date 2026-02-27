"""Database connection pool management."""

import time
from typing import Any, Dict, List, Optional

from ..utils.logging import get_logger
from ..exceptions import DatabaseError

_logger = get_logger("database.pool")

# Pool configuration defaults
DEFAULT_POOL_SIZE = 10
MAX_POOL_SIZE = 50
IDLE_TIMEOUT = 300


class ConnectionHandle:
    """Wrapper around a raw database connection."""

    def __init__(self, conn_id: str, created_at: float):
        self.conn_id = conn_id
        self.created_at = created_at
        self.last_used = created_at
        self.in_use = False
        self.query_count = 0

    def mark_used(self) -> None:
        """Mark connection as actively in use."""
        self.in_use = True
        self.last_used = time.time()
        self.query_count += 1

    def release(self) -> None:
        """Return connection to the pool."""
        self.in_use = False
        self.last_used = time.time()

    def is_stale(self, timeout: int = IDLE_TIMEOUT) -> bool:
        """Check if connection has been idle too long."""
        elapsed = time.time() - self.last_used
        return not self.in_use and elapsed > timeout


class ConnectionPool:
    """Manages a pool of database connections with lifecycle tracking."""

    def __init__(self, dsn: str, pool_size: int = DEFAULT_POOL_SIZE):
        self.dsn = dsn
        self.pool_size = min(pool_size, MAX_POOL_SIZE)
        self._connections: List[ConnectionHandle] = []
        self._initialized = False
        _logger.info(f"Pool created: size={self.pool_size}, dsn={dsn[:20]}...")

    def initialize(self) -> None:
        """Pre-create connections up to pool_size."""
        if self._initialized:
            return
        for i in range(self.pool_size):
            handle = ConnectionHandle(
                conn_id=f"conn-{i}",
                created_at=time.time(),
            )
            self._connections.append(handle)
        self._initialized = True
        _logger.info(f"Pool initialized with {self.pool_size} connections")

    def get_connection(self) -> ConnectionHandle:
        """Acquire a connection from the pool.

        Returns an idle connection or raises DatabaseError if none available.
        """
        if not self._initialized:
            self.initialize()

        # Find an idle connection
        for handle in self._connections:
            if not handle.in_use:
                handle.mark_used()
                _logger.info(f"Acquired connection {handle.conn_id}")
                return handle

        # All connections busy
        _logger.info("No idle connections available")
        raise DatabaseError("Connection pool exhausted")

    def release_connection(self, handle: ConnectionHandle) -> None:
        """Return a connection to the pool."""
        handle.release()
        _logger.info(f"Released connection {handle.conn_id}")

    def cleanup_stale(self) -> int:
        """Remove stale connections and replace them with fresh ones."""
        removed = 0
        for i, handle in enumerate(self._connections):
            if handle.is_stale():
                new_handle = ConnectionHandle(
                    conn_id=f"conn-{i}-refreshed",
                    created_at=time.time(),
                )
                self._connections[i] = new_handle
                removed += 1
        if removed > 0:
            _logger.info(f"Cleaned up {removed} stale connections")
        return removed

    def stats(self) -> Dict[str, Any]:
        """Return pool statistics."""
        active = sum(1 for c in self._connections if c.in_use)
        idle = sum(1 for c in self._connections if not c.in_use)
        total_queries = sum(c.query_count for c in self._connections)
        return {
            "total": len(self._connections),
            "active": active,
            "idle": idle,
            "total_queries": total_queries,
        }

    def shutdown(self) -> None:
        """Close all connections in the pool."""
        for handle in self._connections:
            handle.release()
        self._connections.clear()
        self._initialized = False
        _logger.info("Pool shut down")
