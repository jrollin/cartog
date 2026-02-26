"""Session model."""

from typing import Optional
from datetime import datetime, timedelta

from models.user import User


class Session:
    """Represents an authentication session."""

    _store = {}

    def __init__(self, token: str, user: User, expires_at: datetime):
        self.token = token
        self.user = user
        self.expires_at = expires_at

    @classmethod
    def create(cls, user: User, token: str, expires_in: int = 3600) -> "Session":
        """Create a new session."""
        expires_at = datetime.utcnow() + timedelta(seconds=expires_in)
        session = cls(token=token, user=user, expires_at=expires_at)
        cls._store[token] = session
        return session

    @classmethod
    def find_by_token(cls, token: str) -> Optional["Session"]:
        """Look up a session by token."""
        return cls._store.get(token)

    @classmethod
    def find_all_by_user(cls, user: User) -> list:
        """Find all sessions for a user."""
        return [s for s in cls._store.values() if s.user.id == user.id]

    def delete(self):
        """Delete this session."""
        self._store.pop(self.token, None)

    def is_expired(self) -> bool:
        """Check if this session has expired."""
        return self.expires_at < datetime.utcnow()
