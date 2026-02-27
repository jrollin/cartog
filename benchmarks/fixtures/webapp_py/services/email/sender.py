"""Email sending service with template support."""

import time
from typing import Any, Dict, List, Optional

from ...utils.logging import get_logger
from ...utils.helpers import validate_request, sanitize_input
from ...database.connection import DatabaseConnection
from ...exceptions import AppError, ValidationError
from ..base import BaseService
from ..cacheable import CacheableService

_logger = get_logger("services.email.sender")

# Email templates
TEMPLATES = {
    "welcome": "Welcome to our platform, {name}!",
    "password_reset": "Click here to reset your password: {link}",
    "payment_receipt": "Payment of {amount} {currency} received. Transaction: {txn_id}",
    "notification": "{message}",
    "verification": "Verify your email: {link}",
    "invoice": "Invoice #{invoice_id} for {amount} {currency} is due on {due_date}",
}


class EmailSender(CacheableService):
    """Sends emails via configured transport with template rendering."""

    def __init__(self, db: DatabaseConnection, smtp_host: str = "localhost"):
        super().__init__(db, "email_sender")
        self._smtp_host = smtp_host
        self._sent_count = 0
        self._failed_count = 0
        self._rate_limit = 100  # max emails per minute
        self._last_reset = time.time()
        self._current_count = 0

    def send(
        self, to: str, subject: str, body: str, from_addr: str = "noreply@app.com"
    ) -> bool:
        """Send a single email."""
        self._require_initialized()
        _logger.info(f"Sending email to {to}: {subject}")

        if not self._check_rate_limit():
            _logger.info(f"Rate limit exceeded for email sending")
            self._failed_count += 1
            return False

        # Validate email address
        if "@" not in to or "." not in to:
            raise ValidationError("Invalid email address", field="to")

        # Record in database
        try:
            self._db.insert(
                "notifications",
                {
                    "user_id": "system",
                    "channel": "email",
                    "subject": subject,
                    "body": body,
                    "sent_at": time.strftime("%Y-%m-%dT%H:%M:%SZ"),
                    "status": "sent",
                },
            )
            self._sent_count += 1
            self._current_count += 1
            return True
        except Exception as e:
            _logger.info(f"Email send failed: {e}")
            self._failed_count += 1
            return False

    def send_template(
        self, to: str, template_name: str, context: Dict[str, Any]
    ) -> bool:
        """Send an email using a named template."""
        template = TEMPLATES.get(template_name)
        if not template:
            raise ValidationError(
                f"Unknown template: {template_name}", field="template"
            )

        # Check cache for rendered template
        cache_key = f"email_template:{template_name}:{hash(str(context))}"
        cached = self.cache_get(cache_key)
        if cached:
            body = cached
        else:
            body = template.format(**context)
            self.cache_set(cache_key, body, ttl=3600)

        subject = f"[App] {template_name.replace('_', ' ').title()}"
        return self.send(to, subject, body)

    def send_bulk(
        self, recipients: List[str], subject: str, body: str
    ) -> Dict[str, int]:
        """Send the same email to multiple recipients."""
        _logger.info(f"Bulk sending to {len(recipients)} recipients")
        sent = 0
        failed = 0

        for recipient in recipients:
            clean_addr = sanitize_input(recipient)
            if self.send(clean_addr, subject, body):
                sent += 1
            else:
                failed += 1

        return {"sent": sent, "failed": failed}

    def stats(self) -> Dict[str, Any]:
        """Return email sending statistics."""
        return {
            "sent": self._sent_count,
            "failed": self._failed_count,
            "rate_remaining": self._rate_limit - self._current_count,
        }

    def _check_rate_limit(self) -> bool:
        """Check if we're within the per-minute rate limit."""
        now = time.time()
        if now - self._last_reset >= 60:
            self._current_count = 0
            self._last_reset = now
        return self._current_count < self._rate_limit
