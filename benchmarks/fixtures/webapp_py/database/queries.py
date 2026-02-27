"""Predefined query builders for common database operations."""

from typing import Any, Dict, List, Optional

from ..utils.logging import get_logger
from ..exceptions import NotFoundError, DatabaseError
from .connection import DatabaseConnection, QueryResult

_logger = get_logger("database.queries")


class UserQueries:
    """Query builder for user-related database operations."""

    def __init__(self, db: DatabaseConnection):
        self._db = db

    def find_by_email(self, email: str) -> Optional[Dict[str, Any]]:
        """Look up a user by email address."""
        _logger.info(f"Finding user by email: {email}")
        result = self._db.execute_query(
            "SELECT * FROM users WHERE email = ? AND deleted_at IS NULL",
            (email,),
        )
        return result.first()

    def find_active_users(self, limit: int = 100) -> List[Dict[str, Any]]:
        """Return all active, non-deleted users."""
        result = self._db.execute_query(
            "SELECT * FROM users WHERE active = 1 AND deleted_at IS NULL LIMIT ?",
            (limit,),
        )
        return result.rows

    def count_by_role(self, role: str) -> int:
        """Count users with a specific role."""
        result = self._db.execute_query(
            "SELECT COUNT(*) as cnt FROM users WHERE role = ?",
            (role,),
        )
        row = result.first()
        return row["cnt"] if row else 0

    def search_users(
        self, query: str, page: int = 1, per_page: int = 20
    ) -> QueryResult:
        """Full-text search on user name and email."""
        offset = (page - 1) * per_page
        return self._db.execute_query(
            "SELECT * FROM users WHERE name LIKE ? OR email LIKE ? LIMIT ? OFFSET ?",
            (f"%{query}%", f"%{query}%", per_page, offset),
        )

    def soft_delete(self, user_id: str) -> bool:
        """Soft-delete a user by setting deleted_at timestamp."""
        _logger.info(f"Soft-deleting user {user_id}")
        return self._db.update("users", user_id, {"deleted_at": "NOW()"}) > 0


class SessionQueries:
    """Query builder for session-related database operations."""

    def __init__(self, db: DatabaseConnection):
        self._db = db

    def find_active_session(self, token: str) -> Optional[Dict[str, Any]]:
        """Find an active session by its token hash."""
        result = self._db.execute_query(
            "SELECT * FROM sessions WHERE token_hash = ? AND expired_at IS NULL",
            (token,),
        )
        return result.first()

    def create_session(self, user_id: str, token_hash: str, ip: str) -> str:
        """Create a new session record."""
        _logger.info(f"Creating session for user {user_id}")
        return self._db.insert(
            "sessions",
            {
                "user_id": user_id,
                "token_hash": token_hash,
                "ip_address": ip,
                "created_at": "NOW()",
            },
        )

    def expire_session(self, session_id: str) -> bool:
        """Mark a session as expired."""
        return self._db.update("sessions", session_id, {"expired_at": "NOW()"}) > 0

    def expire_all_for_user(self, user_id: str) -> int:
        """Expire all sessions belonging to a user."""
        _logger.info(f"Expiring all sessions for user {user_id}")
        result = self._db.execute_query(
            "UPDATE sessions SET expired_at = NOW() WHERE user_id = ? AND expired_at IS NULL",
            (user_id,),
        )
        return result.affected

    def count_active(self) -> int:
        """Count currently active sessions."""
        result = self._db.execute_query(
            "SELECT COUNT(*) as cnt FROM sessions WHERE expired_at IS NULL",
            None,
        )
        row = result.first()
        return row["cnt"] if row else 0


class PaymentQueries:
    """Query builder for payment-related database operations."""

    def __init__(self, db: DatabaseConnection):
        self._db = db

    def find_by_transaction_id(self, txn_id: str) -> Optional[Dict[str, Any]]:
        """Look up a payment by transaction ID."""
        result = self._db.execute_query(
            "SELECT * FROM payments WHERE transaction_id = ?",
            (txn_id,),
        )
        return result.first()

    def find_user_payments(
        self, user_id: str, status: Optional[str] = None
    ) -> List[Dict[str, Any]]:
        """Get all payments for a user, optionally filtered by status."""
        _logger.info(f"Finding payments for user {user_id}")
        if status:
            result = self._db.execute_query(
                "SELECT * FROM payments WHERE user_id = ? AND status = ? ORDER BY created_at DESC",
                (user_id, status),
            )
        else:
            result = self._db.execute_query(
                "SELECT * FROM payments WHERE user_id = ? ORDER BY created_at DESC",
                (user_id,),
            )
        return result.rows

    def create_payment(
        self, user_id: str, amount: float, currency: str, txn_id: str
    ) -> str:
        """Record a new payment."""
        return self._db.insert(
            "payments",
            {
                "user_id": user_id,
                "amount": amount,
                "currency": currency,
                "transaction_id": txn_id,
                "status": "pending",
                "created_at": "NOW()",
            },
        )

    def update_status(self, txn_id: str, status: str) -> bool:
        """Update the status of a payment."""
        _logger.info(f"Updating payment {txn_id} status to {status}")
        result = self._db.execute_query(
            "UPDATE payments SET status = ? WHERE transaction_id = ?",
            (status, txn_id),
        )
        return result.affected > 0

    def calculate_revenue(self, start_date: str, end_date: str) -> float:
        """Calculate total revenue in a date range."""
        result = self._db.execute_query(
            "SELECT SUM(amount) as total FROM payments WHERE status = 'completed' AND created_at BETWEEN ? AND ?",
            (start_date, end_date),
        )
        row = result.first()
        return float(row["total"]) if row and row.get("total") else 0.0
