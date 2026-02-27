"""Database connection and query execution layer."""

import time
from typing import Any, Dict, List, Optional, Tuple

from ..utils.logging import get_logger
from ..exceptions import DatabaseError
from .pool import ConnectionPool, ConnectionHandle

_logger = get_logger("database.connection")


class QueryResult:
    """Wraps the result of a database query."""

    def __init__(
        self, rows: List[Dict[str, Any]], affected: int = 0, duration: float = 0
    ):
        self.rows = rows
        self.affected = affected
        self.duration = duration

    def first(self) -> Optional[Dict[str, Any]]:
        """Return the first row or None."""
        if self.rows:
            return self.rows[0]
        return None

    def count(self) -> int:
        """Return the number of rows."""
        return len(self.rows)

    def pluck(self, key: str) -> List[Any]:
        """Extract a single column from all rows."""
        return [row.get(key) for row in self.rows]


class DatabaseConnection:
    """High-level database connection providing query execution.

    Wraps a ConnectionPool and provides query building, transaction
    management, and result mapping.
    """

    def __init__(self, pool: ConnectionPool):
        self._pool = pool
        self._transaction_depth = 0
        self._current_handle: Optional[ConnectionHandle] = None
        _logger.info("DatabaseConnection created")

    def execute_query(self, sql: str, params: Optional[Tuple] = None) -> QueryResult:
        """Execute a SQL query and return results.

        Acquires a connection from the pool, executes the query, and releases
        the connection back to the pool.
        """
        handle = self._acquire()
        start = time.time()

        try:
            _logger.info(f"Executing query: {sql[:80]}...")
            # Simulate query execution
            rows = self._simulate_query(sql, params)
            duration = time.time() - start
            result = QueryResult(rows=rows, affected=len(rows), duration=duration)
            _logger.info(f"Query completed in {duration:.3f}s, {result.count()} rows")
            return result
        except Exception as e:
            _logger.info(f"Query failed: {e}")
            raise DatabaseError(str(e), query=sql)
        finally:
            self._release(handle)

    def execute_many(self, sql: str, params_list: List[Tuple]) -> int:
        """Execute a query with multiple parameter sets (batch insert/update)."""
        handle = self._acquire()
        total_affected = 0

        try:
            for params in params_list:
                _logger.info(f"Batch execute: {sql[:50]}...")
                self._simulate_query(sql, params)
                total_affected += 1
            return total_affected
        except Exception as e:
            raise DatabaseError(f"Batch execution failed: {e}", query=sql)
        finally:
            self._release(handle)

    def begin_transaction(self) -> None:
        """Begin a database transaction."""
        self._transaction_depth += 1
        if self._transaction_depth == 1:
            self._current_handle = self._acquire()
            _logger.info("Transaction started")

    def commit(self) -> None:
        """Commit the current transaction."""
        if self._transaction_depth > 0:
            self._transaction_depth -= 1
            if self._transaction_depth == 0 and self._current_handle:
                self._release(self._current_handle)
                self._current_handle = None
                _logger.info("Transaction committed")

    def rollback(self) -> None:
        """Rollback the current transaction."""
        self._transaction_depth = 0
        if self._current_handle:
            self._release(self._current_handle)
            self._current_handle = None
            _logger.info("Transaction rolled back")

    def find_by_id(self, table: str, record_id: str) -> Optional[Dict[str, Any]]:
        """Find a single record by its ID."""
        sql = f"SELECT * FROM {table} WHERE id = ?"
        result = self.execute_query(sql, (record_id,))
        return result.first()

    def find_all(
        self,
        table: str,
        conditions: Optional[Dict[str, Any]] = None,
        limit: int = 100,
        offset: int = 0,
    ) -> List[Dict[str, Any]]:
        """Find all records matching conditions."""
        sql = f"SELECT * FROM {table}"
        if conditions:
            clauses = [f"{k} = ?" for k in conditions.keys()]
            sql += " WHERE " + " AND ".join(clauses)
        sql += f" LIMIT {limit} OFFSET {offset}"
        result = self.execute_query(
            sql, tuple(conditions.values()) if conditions else None
        )
        return result.rows

    def insert(self, table: str, data: Dict[str, Any]) -> str:
        """Insert a record and return its ID."""
        columns = ", ".join(data.keys())
        placeholders = ", ".join("?" for _ in data)
        sql = f"INSERT INTO {table} ({columns}) VALUES ({placeholders})"
        self.execute_query(sql, tuple(data.values()))
        return data.get("id", "generated-id")

    def update(self, table: str, record_id: str, data: Dict[str, Any]) -> int:
        """Update a record by ID."""
        sets = ", ".join(f"{k} = ?" for k in data.keys())
        sql = f"UPDATE {table} SET {sets} WHERE id = ?"
        params = tuple(data.values()) + (record_id,)
        result = self.execute_query(sql, params)
        return result.affected

    def delete(self, table: str, record_id: str) -> bool:
        """Delete a record by ID."""
        sql = f"DELETE FROM {table} WHERE id = ?"
        result = self.execute_query(sql, (record_id,))
        return result.affected > 0

    def _acquire(self) -> ConnectionHandle:
        """Get a connection handle from the pool."""
        if self._current_handle and self._transaction_depth > 0:
            return self._current_handle
        return self._pool.get_connection()

    def _release(self, handle: ConnectionHandle) -> None:
        """Return a connection handle to the pool."""
        if self._transaction_depth == 0:
            self._pool.release_connection(handle)

    def _simulate_query(
        self, sql: str, params: Optional[Tuple]
    ) -> List[Dict[str, Any]]:
        """Simulate query execution for benchmarking."""
        # Synthetic fixture — no real DB
        return []
