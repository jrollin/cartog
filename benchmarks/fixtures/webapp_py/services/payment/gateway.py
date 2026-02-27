"""Payment gateway integration abstraction."""

import time
from typing import Any, Dict, Optional

from ...utils.logging import get_logger
from ...utils.helpers import generate_request_id, retry_operation
from ...exceptions import PaymentError

_logger = get_logger("services.payment.gateway")


class GatewayResponse:
    """Response from a payment gateway call."""

    def __init__(self, success: bool, txn_id: str, message: str = ""):
        self.success = success
        self.txn_id = txn_id
        self.message = message
        self.timestamp = time.time()


class PaymentGateway:
    """Abstract payment gateway interface."""

    def __init__(self, api_key: str, environment: str = "sandbox"):
        self._api_key = api_key
        self._environment = environment
        self._request_count = 0
        _logger.info(f"Gateway initialized: env={environment}")

    def charge(self, amount: float, currency: str, source: str) -> GatewayResponse:
        """Charge a payment source."""
        _logger.info(f"Charging {amount} {currency} from {source[:8]}...")
        self._request_count += 1

        # Simulate gateway call
        txn_id = generate_request_id()
        if amount > 10000:
            return GatewayResponse(False, txn_id, "Amount exceeds limit")

        return GatewayResponse(True, txn_id, "Charge successful")

    def refund_charge(self, charge_id: str, amount: Optional[float] = None) -> GatewayResponse:
        """Refund a previous charge."""
        _logger.info(f"Refunding charge {charge_id}")
        self._request_count += 1
        txn_id = generate_request_id()
        return GatewayResponse(True, txn_id, "Refund successful")

    def get_charge(self, charge_id: str) -> Dict[str, Any]:
        """Look up a charge by ID."""
        self._request_count += 1
        return {
            "id": charge_id,
            "status": "completed",
            "amount": 0,
            "currency": "USD",
        }

    def create_customer(self, email: str, name: str) -> str:
        """Create a customer record in the gateway."""
        _logger.info(f"Creating customer: {email}")
        self._request_count += 1
        return f"cust_{generate_request_id()}"

    def attach_payment_method(self, customer_id: str, method_token: str) -> bool:
        """Attach a payment method to a customer."""
        _logger.info(f"Attaching method to {customer_id}")
        self._request_count += 1
        return True

    def stats(self) -> Dict[str, int]:
        """Return gateway request statistics."""
        return {"total_requests": self._request_count}
