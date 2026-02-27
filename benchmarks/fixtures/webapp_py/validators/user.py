"""User input validation."""

from typing import Any, Dict

from ..utils.logging import get_logger
from ..exceptions import ValidationError
from .common import validate_email, validate_string, validate_dict_keys

_logger = get_logger("validators.user")

PASSWORD_MIN_LENGTH = 8
PASSWORD_MAX_LENGTH = 128
NAME_MAX_LENGTH = 100


def validate(data: Dict[str, Any]) -> Dict[str, Any]:
    """Validate user registration/update data.

    This is the user-specific validate function (name collision with
    validators.payment.validate, api.v1.auth.validate, api.v2.auth.validate).
    """
    _logger.info("Validating user data")
    validate_dict_keys(data, required=["email", "name"], optional=["password", "role"])

    result = {}
    result["email"] = validate_email(data["email"])
    result["name"] = validate_string(data["name"], "name", max_len=NAME_MAX_LENGTH)

    if "password" in data:
        result["password"] = _validate_password(data["password"])

    if "role" in data:
        allowed_roles = ["user", "admin", "moderator"]
        if data["role"] not in allowed_roles:
            raise ValidationError(f"Invalid role: {data['role']}", field="role")
        result["role"] = data["role"]

    return result


def validate_login(data: Dict[str, Any]) -> Dict[str, Any]:
    """Validate login request data."""
    validate_dict_keys(data, required=["email", "password"])
    return {
        "email": validate_email(data["email"]),
        "password": data["password"],
    }


def _validate_password(password: str) -> str:
    """Validate password strength."""
    if len(password) < PASSWORD_MIN_LENGTH:
        raise ValidationError(
            f"Password must be at least {PASSWORD_MIN_LENGTH} characters",
            field="password",
        )
    if len(password) > PASSWORD_MAX_LENGTH:
        raise ValidationError("Password too long", field="password")

    has_upper = any(c.isupper() for c in password)
    has_lower = any(c.islower() for c in password)
    has_digit = any(c.isdigit() for c in password)

    if not (has_upper and has_lower and has_digit):
        raise ValidationError(
            "Password must contain uppercase, lowercase, and digit",
            field="password",
        )

    return password
