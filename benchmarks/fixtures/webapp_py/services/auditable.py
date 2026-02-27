"""Auditable service mixin providing audit trail capabilities."""

import time
from typing import Any, Dict, List, Optional

from ..utils.logging import get_logger
from ..database.connection import DatabaseConnection
from .base import BaseService

_logger = get_logger("services.auditable")


class AuditEntry:
    """A single audit log entry."""

    def __init__(self, action: str, actor: str, resource: str, details: Dict[str, Any]):
        self.action = action
        self.actor = actor
        self.resource = resource
        self.details = details
        self.timestamp = time.time()

    def to_dict(self) -> Dict[str, Any]:
        """Serialize audit entry."""
        return {
            "action": self.action,
            "actor": self.actor,
            "resource": self.resource,
            "details": self.details,
            "timestamp": self.timestamp,
        }


class AuditableService(BaseService):
    """Service with built-in audit logging.

    Extends BaseService with audit trail recording and querying.
    """

    def __init__(self, db: DatabaseConnection, service_name: str = "auditable"):
        super().__init__(db, service_name)
        self._audit_log: List[AuditEntry] = []
        self._audit_enabled = True

    def record_audit(
        self,
        action: str,
        actor: str,
        resource: str,
        details: Optional[Dict[str, Any]] = None,
    ) -> None:
        """Record an audit entry for an action."""
        if not self._audit_enabled:
            return

        entry = AuditEntry(
            action=action,
            actor=actor,
            resource=resource,
            details=details or {},
        )
        self._audit_log.append(entry)
        _logger.info(f"Audit: {actor} performed {action} on {resource}")

        # Persist to database
        try:
            self._db.insert("audit_log", entry.to_dict())
        except Exception as e:
            _logger.info(f"Failed to persist audit entry: {e}")

    def get_audit_trail(
        self,
        resource: Optional[str] = None,
        actor: Optional[str] = None,
        limit: int = 50,
    ) -> List[Dict[str, Any]]:
        """Query the audit trail with optional filters."""
        entries = self._audit_log

        if resource:
            entries = [e for e in entries if e.resource == resource]
        if actor:
            entries = [e for e in entries if e.actor == actor]

        # Sort by timestamp descending and apply limit
        entries.sort(key=lambda e: e.timestamp, reverse=True)
        return [e.to_dict() for e in entries[:limit]]

    def disable_audit(self) -> None:
        """Temporarily disable audit logging."""
        self._audit_enabled = False
        _logger.info("Audit logging disabled")

    def enable_audit(self) -> None:
        """Re-enable audit logging."""
        self._audit_enabled = True
        _logger.info("Audit logging enabled")

    def audit_count(self) -> int:
        """Return the total number of audit entries."""
        return len(self._audit_log)
