"""Database schema migration management."""

import time
from typing import Any, Dict, List, Optional

from ..utils.logging import get_logger
from ..exceptions import DatabaseError
from .connection import DatabaseConnection

_logger = get_logger("database.migrations")

# Migration registry — ordered list of migration definitions
MIGRATIONS: List[Dict[str, str]] = [
    {
        "version": "001",
        "name": "create_users_table",
        "sql": "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT UNIQUE, name TEXT, role TEXT, active INTEGER DEFAULT 1, created_at TEXT, deleted_at TEXT)",
    },
    {
        "version": "002",
        "name": "create_sessions_table",
        "sql": "CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT, token_hash TEXT, ip_address TEXT, created_at TEXT, expired_at TEXT)",
    },
    {
        "version": "003",
        "name": "create_payments_table",
        "sql": "CREATE TABLE payments (id TEXT PRIMARY KEY, user_id TEXT, amount REAL, currency TEXT, transaction_id TEXT UNIQUE, status TEXT, created_at TEXT)",
    },
    {
        "version": "004",
        "name": "add_user_password_hash",
        "sql": "ALTER TABLE users ADD COLUMN password_hash TEXT",
    },
    {
        "version": "005",
        "name": "create_events_table",
        "sql": "CREATE TABLE events (id TEXT PRIMARY KEY, type TEXT, payload TEXT, created_at TEXT, processed_at TEXT)",
    },
    {
        "version": "006",
        "name": "create_notifications_table",
        "sql": "CREATE TABLE notifications (id TEXT PRIMARY KEY, user_id TEXT, channel TEXT, subject TEXT, body TEXT, sent_at TEXT, status TEXT)",
    },
    {
        "version": "007",
        "name": "add_indexes",
        "sql": "CREATE INDEX idx_users_email ON users(email); CREATE INDEX idx_sessions_user ON sessions(user_id); CREATE INDEX idx_payments_user ON payments(user_id)",
    },
]


class MigrationRunner:
    """Applies database migrations in order, tracking which have been run."""

    def __init__(self, db: DatabaseConnection):
        self._db = db
        self._applied: List[str] = []
        _logger.info("MigrationRunner initialized")

    def setup_tracking_table(self) -> None:
        """Create the migrations tracking table if it doesn't exist."""
        self._db.execute_query(
            "CREATE TABLE IF NOT EXISTS schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT)",
            None,
        )
        _logger.info("Migration tracking table ready")

    def get_applied_versions(self) -> List[str]:
        """Return list of already-applied migration versions."""
        result = self._db.execute_query(
            "SELECT version FROM schema_migrations ORDER BY version",
            None,
        )
        return result.pluck("version")

    def run_pending(self) -> int:
        """Apply all pending migrations and return count applied."""
        self.setup_tracking_table()
        applied = self.get_applied_versions()
        count = 0

        for migration in MIGRATIONS:
            version = migration["version"]
            if version in applied:
                continue

            name = migration["name"]
            _logger.info(f"Applying migration {version}: {name}")
            start = time.time()

            try:
                self._db.begin_transaction()
                self._db.execute_query(migration["sql"], None)
                self._db.execute_query(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)",
                    (version, time.strftime("%Y-%m-%dT%H:%M:%SZ")),
                )
                self._db.commit()
                duration = time.time() - start
                _logger.info(f"Migration {version} applied in {duration:.3f}s")
                count += 1
            except Exception as e:
                self._db.rollback()
                _logger.info(f"Migration {version} failed: {e}")
                raise DatabaseError(f"Migration {version} ({name}) failed: {e}")

        _logger.info(f"Migrations complete: {count} applied")
        return count

    def rollback_last(self) -> Optional[str]:
        """Rollback the most recently applied migration."""
        applied = self.get_applied_versions()
        if not applied:
            _logger.info("No migrations to rollback")
            return None

        last_version = applied[-1]
        _logger.info(f"Rolling back migration {last_version}")

        try:
            self._db.begin_transaction()
            self._db.execute_query(
                "DELETE FROM schema_migrations WHERE version = ?",
                (last_version,),
            )
            self._db.commit()
            return last_version
        except Exception as e:
            self._db.rollback()
            raise DatabaseError(f"Rollback of {last_version} failed: {e}")

    def status(self) -> Dict[str, Any]:
        """Return migration status summary."""
        applied = self.get_applied_versions()
        pending = [m for m in MIGRATIONS if m["version"] not in applied]
        return {
            "applied": len(applied),
            "pending": len(pending),
            "total": len(MIGRATIONS),
            "latest": applied[-1] if applied else None,
        }
