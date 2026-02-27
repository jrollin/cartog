"""User management route handlers."""

from typing import Any, Dict

from ..utils.logging import get_logger
from ..utils.helpers import validate_request, paginate
from ..auth.middleware import auth_required
from ..database.connection import DatabaseConnection
from ..database.queries import UserQueries
from ..validators.user import validate as validate_user_data
from ..exceptions import NotFoundError

_logger = get_logger("routes.users")


def get_user_route(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
    """Get a single user by ID."""
    validate_request(request)
    user_id = request.get("params", {}).get("id", "")
    _logger.info(f"Fetching user {user_id}")

    user = db.find_by_id("users", user_id)
    if not user:
        raise NotFoundError("User", user_id)

    return {"status": 200, "data": user}


def list_users_route(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
    """List users with pagination."""
    validate_request(request)
    queries = UserQueries(db)
    page = int(request.get("params", {}).get("page", 1))

    users = queries.find_active_users(limit=200)
    result = paginate(users, page=page)

    return {"status": 200, "data": result}


def update_user_route(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
    """Update user profile."""
    validate_request(request)
    user_id = request.get("params", {}).get("id", "")
    body = request.get("body", {})

    validated = validate_user_data(body)
    db.update("users", user_id, validated)

    _logger.info(f"Updated user {user_id}")
    return {"status": 200, "data": {"id": user_id, **validated}}


def delete_user_route(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
    """Soft-delete a user."""
    validate_request(request)
    user_id = request.get("params", {}).get("id", "")
    _logger.info(f"Deleting user {user_id}")

    queries = UserQueries(db)
    queries.soft_delete(user_id)

    return {"status": 200, "data": {"deleted": True}}
