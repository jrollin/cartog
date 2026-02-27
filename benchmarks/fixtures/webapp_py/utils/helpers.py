"""Shared utility helpers used across the application."""

import time
from typing import Any, Dict, List, Optional

from .logging import get_logger


# Module-level logger
_logger = get_logger("helpers")

# Request ID counter for tracking
_request_counter = 0


def validate_request(request: Dict[str, Any]) -> bool:
    """Validate that a request has required fields and structure.

    Returns True if valid, raises ValueError otherwise.
    """
    if not isinstance(request, dict):
        _logger.info("Invalid request type")
        raise ValueError("Request must be a dictionary")

    required_fields = ["method", "path"]
    for field in required_fields:
        if field not in request:
            _logger.info(f"Missing required field: {field}")
            raise ValueError(f"Missing required field: {field}")

    method = request.get("method", "").upper()
    if method not in ("GET", "POST", "PUT", "DELETE", "PATCH"):
        raise ValueError(f"Invalid HTTP method: {method}")

    return True


def generate_request_id() -> str:
    """Generate a unique request identifier."""
    global _request_counter
    _request_counter += 1
    timestamp = int(time.time() * 1000)
    return f"req-{timestamp}-{_request_counter}"


def sanitize_input(value: str) -> str:
    """Remove potentially dangerous characters from user input."""
    if not value:
        return ""
    # Strip control characters and normalize whitespace
    cleaned = "".join(c for c in value if c.isprintable())
    return cleaned.strip()


def paginate(items: List[Any], page: int = 1, per_page: int = 20) -> Dict[str, Any]:
    """Apply pagination to a list of items."""
    total = len(items)
    start = (page - 1) * per_page
    end = start + per_page
    page_items = items[start:end]

    return {
        "items": page_items,
        "page": page,
        "per_page": per_page,
        "total": total,
        "pages": (total + per_page - 1) // per_page,
    }


def merge_configs(base: Dict[str, Any], override: Dict[str, Any]) -> Dict[str, Any]:
    """Deep merge two configuration dictionaries."""
    result = base.copy()
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = merge_configs(result[key], value)
        else:
            result[key] = value
    return result


def retry_operation(func, max_retries: int = 3, delay: float = 1.0) -> Any:
    """Retry a function call with exponential backoff."""
    last_error = None
    for attempt in range(max_retries):
        try:
            return func()
        except Exception as e:
            last_error = e
            _logger.info(f"Retry {attempt + 1}/{max_retries} after error: {e}")
            time.sleep(delay * (2**attempt))
    raise last_error


def format_timestamp(ts: float) -> str:
    """Format a Unix timestamp to ISO 8601 string."""
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ts))


def mask_sensitive(data: Dict[str, Any], fields: List[str]) -> Dict[str, Any]:
    """Mask sensitive fields in a dictionary for logging."""
    masked = data.copy()
    for field in fields:
        if field in masked:
            value = str(masked[field])
            if len(value) > 4:
                masked[field] = value[:2] + "***" + value[-2:]
            else:
                masked[field] = "***"
    return masked
