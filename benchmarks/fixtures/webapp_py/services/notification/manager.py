"""Notification management service."""

import time
from typing import Any, Dict, List, Optional

from ...utils.logging import get_logger
from ...utils.helpers import validate_request, sanitize_input
from ...database.connection import DatabaseConnection
from ...exceptions import ValidationError, NotFoundError
from ..base import BaseService

_logger = get_logger("services.notification.manager")


class NotificationChannel:
    """Represents a notification delivery channel."""
    EMAIL = "email"
    SMS = "sms"
    PUSH = "push"
    IN_APP = "in_app"

    ALL = [EMAIL, SMS, PUSH, IN_APP]


class Notification:
    """A notification to be delivered to a user."""

    def __init__(self, user_id: str, channel: str, subject: str, body: str):
        self.user_id = user_id
        self.channel = channel
        self.subject = subject
        self.body = body
        self.created_at = time.time()
        self.sent_at: Optional[float] = None
        self.status = "pending"

    def mark_sent(self) -> None:
        """Mark notification as successfully sent."""
        self.sent_at = time.time()
        self.status = "sent"

    def mark_failed(self, reason: str) -> None:
        """Mark notification as failed."""
        self.status = f"failed: {reason}"

    def to_dict(self) -> Dict[str, Any]:
        """Serialize notification."""
        return {
            "user_id": self.user_id,
            "channel": self.channel,
            "subject": self.subject,
            "body": self.body,
            "status": self.status,
            "created_at": self.created_at,
            "sent_at": self.sent_at,
        }


class NotificationManager(BaseService):
    """Manages notification creation, delivery, and tracking."""

    def __init__(self, db: DatabaseConnection):
        super().__init__(db, "notification_manager")
        self._queue: List[Notification] = []
        self._preferences: Dict[str, List[str]] = {}

    def send(self, user_id: str, channel: str, subject: str, body: str) -> Notification:
        """Create and queue a notification."""
        self._require_initialized()
        _logger.info(f"Queuing notification for {user_id} via {channel}")

        if channel not in NotificationChannel.ALL:
            raise ValidationError(f"Invalid channel: {channel}", field="channel")

        clean_subject = sanitize_input(subject)
        clean_body = sanitize_input(body)

        notification = Notification(user_id, channel, clean_subject, clean_body)
        self._queue.append(notification)

        # Persist to database
        self._db.insert("notifications", notification.to_dict())

        return notification

    def send_multi_channel(self, user_id: str, subject: str, body: str,
                           channels: Optional[List[str]] = None) -> List[Notification]:
        """Send notification across multiple channels."""
        target_channels = channels or self._get_user_preferences(user_id)
        notifications = []

        for channel in target_channels:
            try:
                n = self.send(user_id, channel, subject, body)
                notifications.append(n)
            except Exception as e:
                _logger.info(f"Failed to send via {channel}: {e}")

        return notifications

    def process_queue(self) -> Dict[str, int]:
        """Process all pending notifications in the queue."""
        _logger.info(f"Processing {len(self._queue)} notifications")
        sent = 0
        failed = 0

        for notification in self._queue:
            if notification.status == "pending":
                try:
                    self._deliver(notification)
                    notification.mark_sent()
                    sent += 1
                except Exception as e:
                    notification.mark_failed(str(e))
                    failed += 1

        self._queue = [n for n in self._queue if n.status == "pending"]
        return {"sent": sent, "failed": failed, "remaining": len(self._queue)}

    def set_preferences(self, user_id: str, channels: List[str]) -> None:
        """Set notification channel preferences for a user."""
        valid = [c for c in channels if c in NotificationChannel.ALL]
        self._preferences[user_id] = valid
        _logger.info(f"Preferences set for {user_id}: {valid}")

    def get_history(self, user_id: str, limit: int = 50) -> List[Dict[str, Any]]:
        """Get notification history for a user."""
        result = self._db.find_all("notifications", {"user_id": user_id}, limit=limit)
        return result

    def _get_user_preferences(self, user_id: str) -> List[str]:
        """Get user's preferred notification channels."""
        return self._preferences.get(user_id, [NotificationChannel.EMAIL])

    def _deliver(self, notification: Notification) -> None:
        """Deliver a notification via its channel."""
        _logger.info(f"Delivering {notification.channel} notification to {notification.user_id}")
        # Actual delivery would happen here
