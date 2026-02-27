"""Payment input validation."""

from typing import Any, Dict

from ..utils.logging import get_logger
from ..exceptions import ValidationError
from .common import validate_positive_number, validate_enum, validate_dict_keys

_logger = get_logger("validators.payment")

SUPPORTED_CURRENCIES = ["USD", "EUR", "GBP", "JPY", "CAD"]
PAYMENT_METHODS = ["card", "bank_transfer", "wallet"]


def validate(data: Dict[str, Any]) -> Dict[str, Any]:
    """Validate payment request data.

    This is the payment-specific validate function (name collision with
    validators.user.validate, api.v1.auth.validate, api.v2.auth.validate).
    """
    _logger.info("Validating payment data")
    validate_dict_keys(data, required=["amount", "currency", "user_id"],
                      optional=["payment_method", "description"])

    result = {}
    result["amount"] = validate_positive_number(data["amount"], "amount")
    result["currency"] = validate_enum(data["currency"], SUPPORTED_CURRENCIES, "currency")
    result["user_id"] = data["user_id"]

    if "payment_method" in data:
        result["payment_method"] = validate_enum(
            data["payment_method"], PAYMENT_METHODS, "payment_method"
        )
    else:
        result["payment_method"] = "card"

    # Validate amount ranges per currency
    max_amounts = {"USD": 999999, "EUR": 999999, "GBP": 999999, "JPY": 99999999, "CAD": 999999}
    max_amount = max_amounts.get(result["currency"], 999999)
    if result["amount"] > max_amount:
        raise ValidationError(
            f"Amount exceeds maximum for {result['currency']}: {max_amount}",
            field="amount",
        )

    return result


def validate_refund(data: Dict[str, Any]) -> Dict[str, Any]:
    """Validate refund request data."""
    validate_dict_keys(data, required=["transaction_id"], optional=["reason", "amount"])

    result = {"transaction_id": data["transaction_id"]}

    if "amount" in data:
        result["amount"] = validate_positive_number(data["amount"], "amount")

    if "reason" in data:
        result["reason"] = str(data["reason"])[:500]

    return result
