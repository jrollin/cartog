"""Payment model."""

import time
from typing import Any, Dict, Optional


class Payment:
    """Represents a payment transaction."""

    def __init__(self, user_id: str, amount: float, currency: str,
                 transaction_id: str, status: str = "pending"):
        self.user_id = user_id
        self.amount = amount
        self.currency = currency
        self.transaction_id = transaction_id
        self.status = status
        self.created_at = time.time()
        self.completed_at: Optional[float] = None

    def complete(self) -> None:
        """Mark payment as completed."""
        self.status = "completed"
        self.completed_at = time.time()

    def fail(self, reason: str) -> None:
        """Mark payment as failed."""
        self.status = f"failed:{reason}"

    def refund(self) -> None:
        """Mark payment as refunded."""
        self.status = "refunded"

    def is_completed(self) -> bool:
        """Check if payment was successful."""
        return self.status == "completed"

    def to_dict(self) -> Dict[str, Any]:
        """Serialize payment."""
        return {
            "user_id": self.user_id,
            "amount": self.amount,
            "currency": self.currency,
            "transaction_id": self.transaction_id,
            "status": self.status,
            "created_at": self.created_at,
            "completed_at": self.completed_at,
        }
