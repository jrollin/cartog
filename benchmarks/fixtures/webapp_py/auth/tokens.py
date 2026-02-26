"""Token validation and generation."""

from datetime import datetime, timedelta
from typing import Optional
import hashlib

from models.user import User
from models.session import Session
from config import SECRET_KEY, TOKEN_EXPIRY


class TokenError(Exception):
    """Base exception for token errors."""

    pass


class ExpiredTokenError(TokenError):
    """Raised when a token has expired."""

    pass


class InvalidScopeError(TokenError):
    """Raised when token scope is insufficient."""

    pass


def generate_token(user: User, expires_in: int = TOKEN_EXPIRY) -> str:
    """Generate a new authentication token for a user."""
    payload = f"{user.id}:{datetime.utcnow().isoformat()}"
    token = hashlib.sha256(f"{payload}:{SECRET_KEY}".encode()).hexdigest()
    Session.create(user=user, token=token, expires_in=expires_in)
    return token


def validate_token(token: str) -> Optional[User]:
    """Validate a token and return the associated user.

    Raises:
        ExpiredTokenError: If the token has expired
        TokenError: If the token is invalid
    """
    session = lookup_session(token)
    if session is None:
        raise TokenError("Invalid token")

    if session.expires_at < datetime.utcnow():
        raise ExpiredTokenError("Token has expired")

    return session.user


def lookup_session(token: str) -> Optional[Session]:
    """Look up a session by its token."""
    return Session.find_by_token(token)


def refresh_token(old_token: str) -> str:
    """Refresh an expiring token."""
    user = validate_token(old_token)
    revoke_token(old_token)
    return generate_token(user)


def revoke_token(token: str) -> bool:
    """Revoke a token, invalidating the session."""
    session = lookup_session(token)
    if session:
        session.delete()
        return True
    return False


def revoke_all_tokens(user: User) -> int:
    """Revoke all tokens for a user."""
    sessions = Session.find_all_by_user(user)
    count = 0
    for session in sessions:
        session.delete()
        count += 1
    return count
