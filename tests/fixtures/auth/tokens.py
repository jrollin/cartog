"""Token validation and generation."""

from datetime import datetime, timedelta
from typing import Optional
import hashlib

from .models import User, Session
from .config import SECRET_KEY, TOKEN_EXPIRY


class TokenError(Exception):
    """Base exception for token errors."""

    pass


class ExpiredTokenError(TokenError):
    """Raised when a token has expired."""

    pass


def generate_token(user: User, expires_in: int = 3600) -> str:
    """Generate a new authentication token for a user."""
    payload = f"{user.id}:{datetime.utcnow().isoformat()}"
    token = hashlib.sha256(f"{payload}:{SECRET_KEY}".encode()).hexdigest()
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
    return Session.query.filter_by(token=token).first()


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
