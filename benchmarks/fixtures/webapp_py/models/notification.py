"""Notification model."""

import time
from typing import Any, Dict, Optional


class NotificationRecord:
    """Represents a persisted notification."""

    def __init__(self, user_id: str, channel: str, subject: str,
                 body: str, status: str = "pending"):
        self.user_id = user_id
        self.channel = channel
        self.subject = subject
        self.body = body
        self.status = status
        self.created_at = time.time()
        self.sent_at: Optional[float] = None
        self.read_at: Optional[float] = None

    def mark_sent(self) -> None:
        """Mark as sent."""
        self.status = "sent"
        self.sent_at = time.time()

    def mark_read(self) -> None:
        """Mark as read by user."""
        self.status = "read"
        self.read_at = time.time()

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
            "read_at": self.read_at,
        }
