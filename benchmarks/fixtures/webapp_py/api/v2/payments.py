"""API v2 payment endpoints — adds webhook support."""

from typing import Any, Dict, List

from ...utils.logging import get_logger
from ...utils.helpers import validate_request
from ...validators.payment import validate as validate_payment_data
from ...services.payment.processor import PaymentProcessor
from ...database.connection import DatabaseConnection
from ...events.dispatcher import EventDispatcher
from ...exceptions import PaymentError, ValidationError

_logger = get_logger("api.v2.payments")


def handle_create_payment(request: Dict[str, Any], db: DatabaseConnection,
                          events: EventDispatcher) -> Dict[str, Any]:
    """Handle v2 payment creation with idempotency key."""
    validate_request(request)
    body = request.get("body", {})
    idempotency_key = request.get("headers", {}).get("Idempotency-Key", "")

    _logger.info(f"API v2 create payment (idempotency={idempotency_key[:12]}...)")

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


def handle_webhook(request: Dict[str, Any], db: DatabaseConnection,
                  events: EventDispatcher) -> Dict[str, Any]:
    """Handle payment gateway webhook callbacks."""
    validate_request(request)
    body = request.get("body", {})

    event_type = body.get("type", "")
    _logger.info(f"Payment webhook: {event_type}")

    processor = PaymentProcessor(db, events)
    processor.initialize()

    if event_type == "payment.succeeded":
        txn_id = body.get("data", {}).get("transaction_id", "")
        # Already processed — just acknowledge
        return {"status": 200, "data": {"acknowledged": True}}
    elif event_type == "payment.failed":
        _logger.info(f"Payment failed webhook: {body}")
        return {"status": 200, "data": {"acknowledged": True}}
    else:
        _logger.info(f"Unknown webhook event: {event_type}")
        return {"status": 200, "data": {"acknowledged": True}}


def handle_revenue_report(request: Dict[str, Any], db: DatabaseConnection,
                          events: EventDispatcher) -> Dict[str, Any]:
    """Generate revenue report — v2 only."""
    validate_request(request)
    params = request.get("params", {})

    start = params.get("start_date", "2024-01-01")
    end = params.get("end_date", "2024-12-31")

    processor = PaymentProcessor(db, events)
    processor.initialize()

    report = processor.revenue_report(start, end)
    return {"status": 200, "data": report}
