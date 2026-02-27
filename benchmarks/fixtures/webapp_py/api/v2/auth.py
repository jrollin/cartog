"""API v2 authentication endpoints — improved over v1."""

from typing import Any, Dict

from ...utils.logging import get_logger
from ...utils.helpers import validate_request, sanitize_input
from ...validators.user import validate_login
from ...services.auth_service import AuthenticationService
from ...database.connection import DatabaseConnection
from ...events.dispatcher import EventDispatcher
from ...exceptions import AuthenticationError, ValidationError, RateLimitError

_logger = get_logger("api.v2.auth")


def validate(request: Dict[str, Any]) -> Dict[str, Any]:
    """Validate an API v2 auth request.

    Name collision: same function name as validators.user.validate,
    validators.payment.validate, and api.v1.auth.validate.
    V2 adds stricter validation and rate limit awareness.
    """
    validate_request(request)
    body = request.get("body", {})

    if not body:
        raise ValidationError("Request body is required")

    # V2 requires 'email' (no legacy 'username' support)
    if "email" not in body:
        raise ValidationError("Email is required", field="email")

    # V2 requires content-type header
    content_type = request.get("headers", {}).get("Content-Type", "")
    if "json" not in content_type.lower():
        _logger.info(f"Invalid content type: {content_type}")

    return body


def handle_login(request: Dict[str, Any], db: DatabaseConnection,
                 events: EventDispatcher) -> Dict[str, Any]:
    """Handle v2 login — adds rate limiting and device tracking."""
    _logger.info("API v2 login request")
    body = validate(request)
    login_data = validate_login(body)

    service = AuthenticationService(db, events)
    service.initialize()

    ip = request.get("ip", "unknown")
    user_agent = request.get("headers", {}).get("User-Agent", "")

    result = service.authenticate(login_data["email"], login_data["password"], ip)
    result["api_version"] = "v2"
    result["device"] = user_agent[:100]

    _logger.info(f"V2 login successful: {login_data['email']}")
    return {"status": 200, "data": result}


def handle_token_refresh(request: Dict[str, Any], db: DatabaseConnection,
                         events: EventDispatcher) -> Dict[str, Any]:
    """Handle v2 token refresh — v1 doesn't have this."""
    _logger.info("API v2 token refresh")
    validate_request(request)

    old_token = request.get("token", "")
    if not old_token:
        raise AuthenticationError("Refresh token required")

    service = AuthenticationService(db, events)
    service.initialize()

    user = service.verify_token(old_token)
    if not user:
        raise AuthenticationError("Invalid refresh token")

    # Generate new token pair
    from ...auth.tokens import generate_token
    new_token = generate_token(user)

    return {
        "status": 200,
        "data": {
            "token": new_token,
            "api_version": "v2",
        },
    }
