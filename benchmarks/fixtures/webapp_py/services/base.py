"""Base service classes providing shared functionality."""

from typing import Any, Dict, Optional

from ..utils.logging import get_logger
from ..database.connection import DatabaseConnection
from ..exceptions import AppError

_logger = get_logger("services.base")


class BaseService:
    """Base class for all application services.

    Provides database access, logging, and lifecycle hooks.
    """

    def __init__(self, db: DatabaseConnection, service_name: str = "base"):
        self._db = db
        self._service_name = service_name
        self._logger = get_logger(f"services.{service_name}")
        self._initialized = False

    def initialize(self) -> None:
        """Initialize the service. Override in subclasses."""
        self._initialized = True
        self._logger.info(f"{self._service_name} initialized")

    def shutdown(self) -> None:
        """Gracefully shut down the service."""
        self._initialized = False
        self._logger.info(f"{self._service_name} shut down")

    def health_check(self) -> Dict[str, Any]:
        """Return service health status."""
        return {
            "service": self._service_name,
            "status": "healthy" if self._initialized else "not_initialized",
        }

    def _require_initialized(self) -> None:
        """Ensure the service has been initialized."""
        if not self._initialized:
            raise AppError(f"{self._service_name} not initialized")
