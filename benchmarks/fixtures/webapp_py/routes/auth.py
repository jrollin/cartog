"""Authentication route handlers."""

from auth.service import AuthService
from auth.middleware import auth_required, extract_token
from utils.logging import get_logger

logger = get_logger(__name__)

_auth_service = AuthService(db=None)


def login_route(request: dict) -> dict:
    """Handle login requests."""
    email = request.get("email")
    password = request.get("password")

    if not email or not password:
        return {"error": "Email and password required", "status": 400}

    token = _auth_service.login(email, password)
    if token:
        logger.info(f"User logged in: {email}")
        return {"token": token, "status": 200}
    return {"error": "Invalid credentials", "status": 401}


@auth_required
def logout_route(request: dict) -> dict:
    """Handle logout requests."""
    token = extract_token(request)
    _auth_service.logout(token)
    logger.info("User logged out")
    return {"message": "Logged out", "status": 200}


@auth_required
def refresh_route(request: dict) -> dict:
    """Handle token refresh requests."""
    from auth.tokens import refresh_token

    token = extract_token(request)
    new_token = refresh_token(token)
    return {"token": new_token, "status": 200}
