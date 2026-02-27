"""Background task for payment processing."""

from typing import Any, Dict, List

from ..utils.logging import get_logger
from ..utils.helpers import validate_request
from ..database.connection import DatabaseConnection
from ..database.queries import PaymentQueries
from ..services.payment.processor import PaymentProcessor
from ..events.dispatcher import EventDispatcher
from ..exceptions import PaymentError

_logger = get_logger("tasks.payment")


def process_pending_payments(db: DatabaseConnection,
                             events: EventDispatcher) -> Dict[str, int]:
    """Process all pending payment records."""
    _logger.info("Processing pending payments")

    queries = PaymentQueries(db)
    processor = PaymentProcessor(db, events)
    processor.initialize()

    # Find pending payments
    pending = queries.find_user_payments("", "pending")
    processed = 0
    failed = 0

    for payment in pending:
        try:
            queries.update_status(payment["transaction_id"], "processing")
            queries.update_status(payment["transaction_id"], "completed")
            processed += 1
        except PaymentError as e:
            _logger.info(f"Payment processing failed: {e}")
            queries.update_status(payment["transaction_id"], "failed")
            failed += 1

    _logger.info(f"Payments processed: {processed}, failed: {failed}")
    return {"processed": processed, "failed": failed}


def reconcile_payments(db: DatabaseConnection,
                       events: EventDispatcher) -> Dict[str, Any]:
    """Reconcile payment records with gateway."""
    _logger.info("Reconciling payments")

    queries = PaymentQueries(db)
    processor = PaymentProcessor(db, events)
    processor.initialize()

    # Check for stuck payments
    processing = queries.find_user_payments("", "processing")
    resolved = 0

    for payment in processing:
        _logger.info(f"Checking stuck payment: {payment.get('transaction_id')}")
        # In real system, would check gateway status
        queries.update_status(payment["transaction_id"], "completed")
        resolved += 1

    return {"resolved": resolved, "checked": len(processing)}


def generate_daily_report(db: DatabaseConnection) -> Dict[str, Any]:
    """Generate daily payment summary report."""
    _logger.info("Generating daily payment report")

    queries = PaymentQueries(db)
    revenue = queries.calculate_revenue("today", "today")

    return {
        "date": "today",
        "total_revenue": revenue,
        "report_type": "daily",
    }
