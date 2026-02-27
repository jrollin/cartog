"""Notification route handlers."""

from typing import Any, Dict

from ..utils.logging import get_logger
from ..utils.helpers import validate_request
from ..database.connection import DatabaseConnection
from ..services.notification.manager import NotificationManager

_logger = get_logger("routes.notifications")


def send_notification_route(request: Dict[str, Any],
                             db: DatabaseConnection) -> Dict[str, Any]:
    """Send a notification to a user."""
    validate_request(request)
    body = request.get("body", {})

    manager = NotificationManager(db)
    manager.initialize()

    notification = manager.send(
        user_id=body.get("user_id", ""),
        channel=body.get("channel", "email"),
        subject=body.get("subject", ""),
        body=body.get("body", ""),
    )

    return {"status": 201, "data": notification.to_dict()}


def list_notifications_route(request: Dict[str, Any],
                              db: DatabaseConnection) -> Dict[str, Any]:
    """List notifications for the authenticated user."""
    validate_request(request)
    user_id = request.get("user", {}).get("user_id", "")

    manager = NotificationManager(db)
    manager.initialize()

    history = manager.get_history(user_id)
    return {"status": 200, "data": history}
