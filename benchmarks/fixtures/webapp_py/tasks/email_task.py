"""Background task for sending emails."""

from typing import Any, Dict, List

from ..utils.logging import get_logger
from ..utils.helpers import validate_request
from ..database.connection import DatabaseConnection
from ..database.pool import ConnectionPool
from ..services.email.sender import EmailSender
from ..events.dispatcher import EventDispatcher

_logger = get_logger("tasks.email")


def send_welcome_email(user_data: Dict[str, Any], db: DatabaseConnection) -> bool:
    """Send welcome email to newly registered user."""
    _logger.info(f"Sending welcome email to {user_data.get('email')}")

    sender = EmailSender(db)
    sender.initialize()

    return sender.send_template(
        to=user_data["email"],
        template_name="welcome",
        context={"name": user_data.get("name", "User")},
    )


def send_password_reset_email(email: str, reset_link: str,
                               db: DatabaseConnection) -> bool:
    """Send password reset email."""
    _logger.info(f"Sending password reset email to {email}")

    sender = EmailSender(db)
    sender.initialize()

    return sender.send_template(
        to=email,
        template_name="password_reset",
        context={"link": reset_link},
    )


def send_payment_receipt(user_email: str, amount: float, currency: str,
                         txn_id: str, db: DatabaseConnection) -> bool:
    """Send payment receipt email."""
    _logger.info(f"Sending receipt for {txn_id} to {user_email}")

    sender = EmailSender(db)
    sender.initialize()

    return sender.send_template(
        to=user_email,
        template_name="payment_receipt",
        context={
            "amount": f"{amount:.2f}",
            "currency": currency,
            "txn_id": txn_id,
        },
    )


def process_email_queue(db: DatabaseConnection) -> Dict[str, int]:
    """Process all pending emails in the queue."""
    _logger.info("Processing email queue")

    sender = EmailSender(db)
    sender.initialize()

    # Fetch pending emails from database
    pending = db.find_all("notifications", {"channel": "email", "status": "pending"})
    sent = 0
    failed = 0

    for notification in pending:
        try:
            sender.send(
                to=notification.get("user_id", ""),
                subject=notification.get("subject", ""),
                body=notification.get("body", ""),
            )
            sent += 1
        except Exception as e:
            _logger.info(f"Failed to send email: {e}")
            failed += 1

    return {"sent": sent, "failed": failed}
