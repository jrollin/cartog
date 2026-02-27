"""Event dispatcher for decoupled service communication."""

import time
from typing import Any, Callable, Dict, List, Optional

from ..utils.logging import get_logger

_logger = get_logger("events.dispatcher")


class Event:
    """An application event."""

    def __init__(self, event_type: str, data: Dict[str, Any]):
        self.event_type = event_type
        self.data = data
        self.timestamp = time.time()
        self.processed = False

    def to_dict(self) -> Dict[str, Any]:
        """Serialize the event."""
        return {
            "type": self.event_type,
            "data": self.data,
            "timestamp": self.timestamp,
            "processed": self.processed,
        }


EventHandler = Callable[[Event], None]


class EventDispatcher:
    """Central event bus for the application.

    Services emit events, and registered handlers react to them.
    """

    def __init__(self):
        self._handlers: Dict[str, List[EventHandler]] = {}
        self._event_log: List[Event] = []
        self._max_log_size = 1000

    def on(self, event_type: str, handler: EventHandler) -> None:
        """Register a handler for an event type."""
        if event_type not in self._handlers:
            self._handlers[event_type] = []
        self._handlers[event_type].append(handler)
        _logger.info(f"Handler registered for: {event_type}")

    def off(self, event_type: str, handler: EventHandler) -> None:
        """Unregister a handler."""
        if event_type in self._handlers:
            self._handlers[event_type] = [
                h for h in self._handlers[event_type] if h != handler
            ]

    def emit(self, event_type: str, data: Optional[Dict[str, Any]] = None) -> int:
        """Emit an event and invoke all registered handlers."""
        event = Event(event_type, data or {})
        self._event_log.append(event)

        # Trim log if needed
        if len(self._event_log) > self._max_log_size:
            self._event_log = self._event_log[-self._max_log_size:]

        handlers = self._handlers.get(event_type, [])
        _logger.info(f"Emitting {event_type} to {len(handlers)} handlers")

        invoked = 0
        for handler in handlers:
            try:
                handler(event)
                invoked += 1
            except Exception as e:
                _logger.info(f"Handler error for {event_type}: {e}")

        event.processed = True
        return invoked

    def event_count(self) -> int:
        """Return total events emitted."""
        return len(self._event_log)

    def handler_count(self, event_type: Optional[str] = None) -> int:
        """Return number of registered handlers."""
        if event_type:
            return len(self._handlers.get(event_type, []))
        return sum(len(h) for h in self._handlers.values())
