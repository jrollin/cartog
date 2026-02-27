"""API v1 authentication endpoints."""

from typing import Any, Dict

from ...utils.logging import get_logger
from ...utils.helpers import validate_request, sanitize_input
from ...validators.user import validate_login
from ...services.auth_service import AuthenticationService
from ...database.connection import DatabaseConnection
from ...events.dispatcher import EventDispatcher
from ...exceptions import AuthenticationError, ValidationError

_logger = get_logger("api.v1.auth")


def validate(request: Dict[str, Any]) -> Dict[str, Any]:
    """Validate an API v1 auth request.

    Name collision: same function name as validators.user.validate,
    validators.payment.validate, and api.v2.auth.validate.
    """
    validate_request(request)
    body = request.get("body", {})

    if not body:
        raise ValidationError("Request body is required")

    # V1 requires 'username' field (legacy)
    if "username" in body and "email" not in body:
        body["email"] = body["username"]

    return body


def handle_login(request: Dict[str, Any], db: DatabaseConnection,
                 events: EventDispatcher) -> Dict[str, Any]:
    """Handle v1 login request — entry point for deep call chain.

    Call chain: handle_login -> authenticate -> login -> generate_token
                -> execute_query -> get_connection
    """
    _logger.info("API v1 login request")
    body = validate(request)
    login_data = validate_login(body)

    service = AuthenticationService(db, events)
    service.initialize()

    ip = request.get("ip", "unknown")
    result = service.authenticate(login_data["email"], login_data["password"], ip)

    _logger.info(f"Login successful: {login_data['email']}")
    return {"status": 200, "data": result}


def handle_register(request: Dict[str, Any], db: DatabaseConnection,
                    events: EventDispatcher) -> Dict[str, Any]:
    """Handle v1 registration request."""
    _logger.info("API v1 register request")
    body = validate(request)

    service = AuthenticationService(db, events)
    service.initialize()

    result = service.register(
        email=sanitize_input(body.get("email", "")),
        password=body.get("password", ""),
        name=sanitize_input(body.get("name", "")),
    )

    return {"status": 201, "data": result}


def handle_logout(request: Dict[str, Any], db: DatabaseConnection,
                  events: EventDispatcher) -> Dict[str, Any]:
    """Handle v1 logout request."""
    _logger.info("API v1 logout request")
    token = request.get("token", "")

    service = AuthenticationService(db, events)
    service.initialize()
    service.logout(token)

    return {"status": 200, "data": {"message": "Logged out"}}
