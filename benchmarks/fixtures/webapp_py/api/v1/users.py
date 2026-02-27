"""API v1 user management endpoints."""

from typing import Any, Dict, List

from ...utils.logging import get_logger
from ...utils.helpers import validate_request, paginate
from ...validators.user import validate as validate_user_data
from ...database.connection import DatabaseConnection
from ...database.queries import UserQueries
from ...exceptions import NotFoundError, AuthorizationError

_logger = get_logger("api.v1.users")


def handle_get_user(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
    """Get a user by ID."""
    validate_request(request)
    user_id = request.get("params", {}).get("id", "")
    _logger.info(f"Getting user: {user_id}")

    queries = UserQueries(db)
    user = db.find_by_id("users", user_id)

    if not user:
        raise NotFoundError("User", user_id)

    return {"status": 200, "data": user}


def handle_update_user(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
    """Update user profile."""
    validate_request(request)
    user_id = request.get("params", {}).get("id", "")
    body = request.get("body", {})
    _logger.info(f"Updating user: {user_id}")

    # Validate and sanitize
    validated = validate_user_data(body)
    db.update("users", user_id, validated)

    return {"status": 200, "data": {"id": user_id, **validated}}


def handle_list_users(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
    """List users with pagination."""
    validate_request(request)
    page = int(request.get("params", {}).get("page", 1))
    per_page = int(request.get("params", {}).get("per_page", 20))
    _logger.info(f"Listing users: page={page}")

    queries = UserQueries(db)
    users = queries.find_active_users(limit=per_page * 10)
    result = paginate(users, page=page, per_page=per_page)

    return {"status": 200, "data": result}


def handle_search_users(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
    """Search users by name or email."""
    validate_request(request)
    query = request.get("params", {}).get("q", "")
    _logger.info(f"Searching users: q={query}")

    queries = UserQueries(db)
    result = queries.search_users(query)

    return {"status": 200, "data": result.rows}
