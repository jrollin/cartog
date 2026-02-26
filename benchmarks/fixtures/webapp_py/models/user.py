"""User model."""

from typing import Optional
from utils.crypto import hash_password, verify_password


class User:
    """Represents an application user."""

    def __init__(self, id: int, email: str, password_hash: str, is_admin: bool = False):
        self.id = id
        self.email = email
        self.password_hash = password_hash
        self.is_admin = is_admin

    def check_password(self, password: str) -> bool:
        """Verify a password against the stored hash."""
        return verify_password(password, self.password_hash)

    def set_password(self, password: str):
        """Set a new password."""
        self.password_hash = hash_password(password)

    @classmethod
    def find_by_email(cls, db, email: str) -> Optional["User"]:
        """Find a user by email address."""
        row = db.query("SELECT * FROM users WHERE email = ?", (email,))
        if row:
            return cls(**row)
        return None

    @classmethod
    def find_by_id(cls, db, user_id: int) -> Optional["User"]:
        """Find a user by ID."""
        row = db.query("SELECT * FROM users WHERE id = ?", (user_id,))
        if row:
            return cls(**row)
        return None

    @classmethod
    def find_all(cls, db) -> list:
        """Return all users."""
        rows = db.query_all("SELECT * FROM users")
        return [cls(**row) for row in rows]
