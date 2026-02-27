"""API v1 payment endpoints."""

from typing import Any, Dict

from ...utils.logging import get_logger
from ...utils.helpers import validate_request
from ...validators.payment import validate as validate_payment_data
from ...validators.payment import validate_refund
from ...services.payment.processor import PaymentProcessor
from ...database.connection import DatabaseConnection
from ...events.dispatcher import EventDispatcher
from ...exceptions import PaymentError, ValidationError

_logger = get_logger("api.v1.payments")


def handle_create_payment(request: Dict[str, Any], db: DatabaseConnection,
                          events: EventDispatcher) -> Dict[str, Any]:
    """Handle payment creation."""
    _logger.info("API v1 create payment")
    validate_request(request)
    body = request.get("body", {})

    payment_data = validate_payment_data(body)

    processor = PaymentProcessor(db, events)
    processor.initialize()

    result = processor.process_payment(
        user_id=payment_data["user_id"],
        amount=payment_data["amount"],
        currency=payment_data["currency"],
        payment_method=payment_data.get("payment_method", "card"),
    )

    return {"status": 201, "data": result}


def handle_refund(request: Dict[str, Any], db: DatabaseConnection,
                  events: EventDispatcher) -> Dict[str, Any]:
    """Handle payment refund."""
    _logger.info("API v1 refund")
    validate_request(request)
    body = request.get("body", {})

    refund_data = validate_refund(body)

    processor = PaymentProcessor(db, events)
    processor.initialize()

    result = processor.refund(
        transaction_id=refund_data["transaction_id"],
        reason=refund_data.get("reason", ""),
    )

    return {"status": 200, "data": result}


def handle_list_payments(request: Dict[str, Any], db: DatabaseConnection,
                         events: EventDispatcher) -> Dict[str, Any]:
    """List payments for the authenticated user."""
    _logger.info("API v1 list payments")
    validate_request(request)
    user_id = request.get("user", {}).get("user_id", "")

    processor = PaymentProcessor(db, events)
    processor.initialize()

    payments = processor.get_user_payments(user_id)
    return {"status": 200, "data": payments}
