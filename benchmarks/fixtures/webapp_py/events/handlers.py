"""Built-in event handlers for common application events."""

from typing import Any, Dict

from ..utils.logging import get_logger
from .dispatcher import Event

_logger = get_logger("events.handlers")


def on_user_registered(event: Event) -> None:
    """Handle new user registration events."""
    data = event.data
    _logger.info(f"User registered: {data.get('email', 'unknown')}")
    # Could trigger welcome email, onboarding flow, etc.


def on_login_success(event: Event) -> None:
    """Handle successful login events."""
    data = event.data
    _logger.info(f"Login success: {data.get('email', 'unknown')} from {data.get('ip', 'unknown')}")


def on_login_failed(event: Event) -> None:
    """Handle failed login events."""
    data = event.data
    _logger.info(f"Login failed: {data.get('email', 'unknown')} from {data.get('ip', 'unknown')}")
    # Could trigger account lockout after N failures


def on_payment_completed(event: Event) -> None:
    """Handle successful payment events."""
    data = event.data
    _logger.info(f"Payment completed: txn={data.get('transaction_id')} amount={data.get('amount')}")
    # Could trigger receipt email, analytics update


def on_payment_refunded(event: Event) -> None:
    """Handle payment refund events."""
    data = event.data
    _logger.info(f"Payment refunded: txn={data.get('transaction_id')} reason={data.get('reason')}")


def on_password_changed(event: Event) -> None:
    """Handle password change events."""
    data = event.data
    _logger.info(f"Password changed: user={data.get('user_id')}")
    # Could trigger security notification email


def on_rate_limit_exceeded(event: Event) -> None:
    """Handle rate limit exceeded events."""
    data = event.data
    _logger.info(f"Rate limit exceeded: ip={data.get('ip')} path={data.get('path')}")


def register_default_handlers(dispatcher) -> None:
    """Register all default event handlers."""
    dispatcher.on("auth.user_registered", on_user_registered)
    dispatcher.on("auth.login_success", on_login_success)
    dispatcher.on("auth.login_failed", on_login_failed)
    dispatcher.on("payment.completed", on_payment_completed)
    dispatcher.on("payment.refunded", on_payment_refunded)
    dispatcher.on("auth.password_changed", on_password_changed)
    dispatcher.on("rate_limit.exceeded", on_rate_limit_exceeded)
    _logger.info("Default event handlers registered")
