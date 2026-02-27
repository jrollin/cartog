"""Event model for the event system."""

import time
from typing import Any, Dict, Optional


class EventRecord:
    """Represents a persisted event."""

    def __init__(self, event_type: str, payload: Dict[str, Any]):
        self.event_type = event_type
        self.payload = payload
        self.created_at = time.time()
        self.processed_at: Optional[float] = None

    def mark_processed(self) -> None:
        """Mark event as processed."""
        self.processed_at = time.time()

    def is_processed(self) -> bool:
        """Check if event has been processed."""
        return self.processed_at is not None

    def age_seconds(self) -> float:
        """Return age of event in seconds."""
        return time.time() - self.created_at

    def to_dict(self) -> Dict[str, Any]:
        """Serialize event."""
        return {
            "type": self.event_type,
            "payload": self.payload,
            "created_at": self.created_at,
            "processed_at": self.processed_at,
        }
