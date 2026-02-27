"""Common validation utilities shared across validators."""

import re
from typing import Any, Dict, List, Optional

from ..utils.logging import get_logger
from ..exceptions import ValidationError

_logger = get_logger("validators.common")

EMAIL_REGEX = re.compile(r'^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$')
URL_REGEX = re.compile(r'^https?://[\w.-]+(?:\.[\w.-]+)+[\w.,@?^=%&:/~+#-]*$')


def validate_email(email: str) -> str:
    """Validate and normalize an email address."""
    if not email or not isinstance(email, str):
        raise ValidationError("Email is required", field="email")

    clean = email.strip().lower()
    if not EMAIL_REGEX.match(clean):
        raise ValidationError(f"Invalid email format: {email}", field="email")

    return clean


def validate_string(value: str, field: str, min_len: int = 1, max_len: int = 255) -> str:
    """Validate a string field for length constraints."""
    if not value or not isinstance(value, str):
        raise ValidationError(f"{field} is required", field=field)

    stripped = value.strip()
    if len(stripped) < min_len:
        raise ValidationError(f"{field} must be at least {min_len} characters", field=field)
    if len(stripped) > max_len:
        raise ValidationError(f"{field} must be at most {max_len} characters", field=field)

    return stripped


def validate_positive_number(value: Any, field: str) -> float:
    """Validate that a value is a positive number."""
    try:
        num = float(value)
    except (TypeError, ValueError):
        raise ValidationError(f"{field} must be a number", field=field)

    if num <= 0:
        raise ValidationError(f"{field} must be positive", field=field)

    return num


def validate_enum(value: str, allowed: List[str], field: str) -> str:
    """Validate that a value is in an allowed set."""
    if value not in allowed:
        raise ValidationError(
            f"Invalid {field}: '{value}'. Allowed: {', '.join(allowed)}",
            field=field,
        )
    return value


def validate_dict_keys(data: Dict[str, Any], required: List[str], optional: Optional[List[str]] = None) -> None:
    """Validate that a dictionary contains required keys."""
    for key in required:
        if key not in data:
            raise ValidationError(f"Missing required field: {key}", field=key)

    allowed = set(required + (optional or []))
    for key in data:
        if key not in allowed:
            _logger.info(f"Unknown field ignored: {key}")
