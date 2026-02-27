"""Authentication middleware."""

from typing import Any, Dict, Optional

from ..utils.logging import get_logger
from ..utils.helpers import validate_request
from ..auth.tokens import validate_token
from ..exceptions import AuthenticationError, AuthorizationError

_logger = get_logger("middleware.auth")

PUBLIC_PATHS = ["/health", "/login", "/register", "/docs"]


def auth_middleware(request: Dict[str, Any]) -> Dict[str, Any]:
    """Verify authentication token and attach user context."""
    validate_request(request)

    path = request.get("path", "")
    if path in PUBLIC_PATHS:
        return request

    token = _extract_token(request)
    if not token:
        raise AuthenticationError("Missing authentication token")

    try:
        claims = validate_token(token)
    except Exception as e:
        _logger.info(f"Token validation failed: {e}")
        raise AuthenticationError("Invalid or expired token")

    request["user"] = claims
    request["authenticated"] = True
    _logger.info(f"Authenticated: user={claims.get('user_id', 'unknown')}")

    return request


def require_role(request: Dict[str, Any], required_role: str) -> None:
    """Verify the authenticated user has the required role."""
    user = request.get("user", {})
    user_role = user.get("role", "user")

    role_hierarchy = {"admin": 3, "moderator": 2, "user": 1}
    if role_hierarchy.get(user_role, 0) < role_hierarchy.get(required_role, 0):
        raise AuthorizationError(required_role, request.get("path", "unknown"))


def _extract_token(request: Dict[str, Any]) -> Optional[str]:
    """Extract bearer token from request headers."""
    auth_header = request.get("headers", {}).get("Authorization", "")
    if auth_header.startswith("Bearer "):
        return auth_header[7:]
    return request.get("token")
