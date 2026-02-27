"""Payment route handlers."""

from typing import Any, Dict

from ..utils.logging import get_logger
from ..utils.helpers import validate_request
from ..auth.middleware import auth_required, extract_token
from ..services.payment.processor import PaymentProcessor
from ..database.connection import DatabaseConnection
from ..events.dispatcher import EventDispatcher
from ..exceptions import PaymentError

_logger = get_logger("routes.payments")


def create_payment_route(request: Dict[str, Any], db: DatabaseConnection,
                         events: EventDispatcher) -> Dict[str, Any]:
    """Route handler for creating a payment."""
    validate_request(request)
    token = extract_token(request)

    processor = PaymentProcessor(db, events)
    processor.initialize()

    body = request.get("body", {})
    result = processor.process_payment(
        user_id=body.get("user_id", ""),
        amount=float(body.get("amount", 0)),
        currency=body.get("currency", "USD"),
    )

    return {"status": 201, "data": result}


def refund_payment_route(request: Dict[str, Any], db: DatabaseConnection,
                         events: EventDispatcher) -> Dict[str, Any]:
    """Route handler for refunding a payment."""
    validate_request(request)

    body = request.get("body", {})
    txn_id = body.get("transaction_id", "")

    processor = PaymentProcessor(db, events)
    processor.initialize()

    try:
        result = processor.refund(txn_id, reason=body.get("reason", ""))
        return {"status": 200, "data": result}
    except PaymentError as e:
        _logger.info(f"Refund failed: {e}")
        return {"status": 400, "error": str(e)}
