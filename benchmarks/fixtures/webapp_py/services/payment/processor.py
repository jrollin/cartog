"""Payment processing service with diamond inheritance."""

import time
from typing import Any, Dict, List, Optional

from ...utils.logging import get_logger
from ...utils.helpers import validate_request, generate_request_id
from ...database.connection import DatabaseConnection
from ...database.queries import PaymentQueries
from ...exceptions import PaymentError, ValidationError, NotFoundError
from ...events.dispatcher import EventDispatcher
from ..cacheable import CacheableService
from ..auditable import AuditableService

_logger = get_logger("services.payment.processor")

SUPPORTED_CURRENCIES = ["USD", "EUR", "GBP", "JPY", "CAD"]
MIN_AMOUNT = 0.50
MAX_AMOUNT = 999999.99


class PaymentProcessor(CacheableService, AuditableService):
    """Processes payments with caching and audit trail.

    Diamond inheritance: CacheableService + AuditableService both extend BaseService.
    """

    def __init__(self, db: DatabaseConnection, events: EventDispatcher):
        CacheableService.__init__(self, db, "payment_processor")
        AuditableService.__init__(self, db, "payment_processor")
        self._events = events
        self._queries = PaymentQueries(db)
        self._processing_count = 0

    def process_payment(self, user_id: str, amount: float, currency: str,
                        payment_method: str = "card") -> Dict[str, Any]:
        """Process a single payment."""
        self._require_initialized()
        _logger.info(f"Processing payment: user={user_id}, amount={amount} {currency}")

        # Validate
        self._validate_payment(amount, currency)

        # Generate transaction ID
        txn_id = generate_request_id()

        # Check cache for duplicate prevention
        cache_key = f"payment:{user_id}:{amount}:{currency}"
        cached = self.cache_get(cache_key)
        if cached:
            _logger.info(f"Duplicate payment detected: {cache_key}")
            raise PaymentError("Duplicate payment detected", transaction_id=txn_id)

        # Record in database
        try:
            self._queries.create_payment(user_id, amount, currency, txn_id)
            self._queries.update_status(txn_id, "completed")
            self._processing_count += 1
        except Exception as e:
            _logger.info(f"Payment failed: {e}")
            raise PaymentError(f"Payment processing failed: {e}", transaction_id=txn_id)

        # Cache to prevent duplicates
        self.cache_set(cache_key, txn_id, ttl=300)

        # Audit trail
        self.record_audit("payment.processed", user_id, f"payment:{txn_id}", {
            "amount": amount,
            "currency": currency,
            "method": payment_method,
        })

        # Emit event
        self._events.emit("payment.completed", {
            "transaction_id": txn_id,
            "user_id": user_id,
            "amount": amount,
            "currency": currency,
        })

        return {
            "transaction_id": txn_id,
            "status": "completed",
            "amount": amount,
            "currency": currency,
        }

    def refund(self, transaction_id: str, reason: str = "") -> Dict[str, Any]:
        """Refund a completed payment."""
        _logger.info(f"Refunding payment: {transaction_id}")

        payment = self._queries.find_by_transaction_id(transaction_id)
        if not payment:
            raise NotFoundError("Payment", transaction_id)

        self._queries.update_status(transaction_id, "refunded")
        self.record_audit("payment.refunded", "system", f"payment:{transaction_id}", {
            "reason": reason,
        })
        self._events.emit("payment.refunded", {
            "transaction_id": transaction_id,
            "reason": reason,
        })

        return {"transaction_id": transaction_id, "status": "refunded"}

    def get_user_payments(self, user_id: str, status: Optional[str] = None) -> List[Dict[str, Any]]:
        """Get all payments for a user."""
        return self._queries.find_user_payments(user_id, status)

    def revenue_report(self, start_date: str, end_date: str) -> Dict[str, Any]:
        """Generate a revenue report for a date range."""
        _logger.info(f"Revenue report: {start_date} to {end_date}")
        total = self._queries.calculate_revenue(start_date, end_date)
        return {
            "start_date": start_date,
            "end_date": end_date,
            "total_revenue": total,
            "payments_processed": self._processing_count,
        }

    def _validate_payment(self, amount: float, currency: str) -> None:
        """Validate payment amount and currency."""
        if currency not in SUPPORTED_CURRENCIES:
            raise ValidationError(f"Unsupported currency: {currency}", field="currency")
        if amount < MIN_AMOUNT:
            raise ValidationError(f"Amount below minimum: {amount}", field="amount")
        if amount > MAX_AMOUNT:
            raise ValidationError(f"Amount above maximum: {amount}", field="amount")
