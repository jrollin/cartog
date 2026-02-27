"""Request/response logging middleware."""

import time
from typing import Any, Dict

from ..utils.logging import get_logger
from ..utils.helpers import validate_request, generate_request_id, mask_sensitive

_logger = get_logger("middleware.logging")

SENSITIVE_FIELDS = ["password", "token", "secret", "api_key", "authorization"]


def logging_middleware(request: Dict[str, Any]) -> Dict[str, Any]:
    """Log incoming requests with timing and request ID."""
    validate_request(request)

    # Assign request ID
    request_id = request.get("request_id") or generate_request_id()
    request["request_id"] = request_id

    # Log sanitized request
    safe_request = mask_sensitive(request, SENSITIVE_FIELDS)
    method = request.get("method", "?")
    path = request.get("path", "?")
    _logger.info(f"[{request_id}] {method} {path}")

    # Record timing
    request["_start_time"] = time.time()
    return request


def log_response(request: Dict[str, Any], status: int, body_size: int = 0) -> None:
    """Log response details with timing."""
    request_id = request.get("request_id", "unknown")
    start = request.get("_start_time", time.time())
    duration = (time.time() - start) * 1000  # milliseconds

    method = request.get("method", "?")
    path = request.get("path", "?")
    _logger.info(f"[{request_id}] {method} {path} -> {status} ({duration:.1f}ms, {body_size}B)")
