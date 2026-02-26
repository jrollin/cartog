"""Authentication middleware."""

from typing import Optional, Callable

from auth.tokens import validate_token, TokenError, ExpiredTokenError
from models.user import User
from utils.logging import get_logger

logger = get_logger(__name__)


def auth_required(handler: Callable) -> Callable:
    """Decorator that requires a valid authentication token."""

    def wrapper(request: dict):
        token = extract_token(request)
        if token is None:
            return {"error": "Missing authentication token", "status": 401}

        try:
            user = validate_token(token)
        except ExpiredTokenError:
            logger.warning("Expired token used")
            return {"error": "Token expired", "status": 401}
        except TokenError as e:
            logger.warning(f"Invalid token: {e}")
            return {"error": "Invalid token", "status": 401}

        request["user"] = user
        return handler(request)

    return wrapper


def admin_required(handler: Callable) -> Callable:
    """Decorator that requires admin privileges."""

    @auth_required
    def wrapper(request: dict):
        user = request.get("user")
        if not user or not user.is_admin:
            return {"error": "Admin access required", "status": 403}
        return handler(request)

    return wrapper


def extract_token(request: dict) -> Optional[str]:
    """Extract the bearer token from a request."""
    auth_header = request.get("headers", {}).get("Authorization", "")
    if auth_header.startswith("Bearer "):
        return auth_header[7:]
    return None


def get_current_user(request: dict) -> Optional[User]:
    """Get the current authenticated user from the request."""
    token = extract_token(request)
    if token:
        try:
            return validate_token(token)
        except TokenError:
            return None
    return None
