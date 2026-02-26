"""Admin route handlers."""

from auth.service import AdminService
from auth.middleware import admin_required, extract_token
from utils.logging import get_logger

logger = get_logger(__name__)

_admin_service = AdminService(db=None)


@admin_required
def impersonate_route(request: dict) -> dict:
    """Handle admin impersonation requests."""
    token = extract_token(request)
    user_id = request.get("user_id")

    if not user_id:
        return {"error": "user_id required", "status": 400}

    try:
        new_token = _admin_service.impersonate(token, user_id)
        logger.info(f"Admin impersonating user {user_id}")
        return {"token": new_token, "status": 200}
    except PermissionError as e:
        return {"error": str(e), "status": 403}


@admin_required
def list_users_route(request: dict) -> dict:
    """Handle list users request."""
    token = extract_token(request)

    try:
        users = _admin_service.list_all_users(token)
        return {"users": [{"id": u.id, "email": u.email} for u in users], "status": 200}
    except PermissionError as e:
        return {"error": str(e), "status": 403}
