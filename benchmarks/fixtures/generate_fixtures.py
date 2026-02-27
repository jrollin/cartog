#!/usr/bin/env python3
"""Generate expanded benchmark fixtures for all 5 languages.

Creates ~5-8K LOC per language with:
- Name collisions (validate in 4+ files)
- High-fanout utilities (get_logger in 25+ files)
- Deep call chains (5+ hops)
- Diamond/deep inheritance
- Deep transitive impact targets
- Rich type annotations
- Custom exception hierarchies
"""

import os
import textwrap

BASE = os.path.dirname(os.path.abspath(__file__))


def write(path, content):
    """Write file, creating directories as needed."""
    full = os.path.join(BASE, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w") as f:
        f.write(textwrap.dedent(content).lstrip())


# ============================================================================
# PYTHON FIXTURE EXPANSION (new files only — don't overwrite existing)
# ============================================================================
def gen_python():
    prefix = "webapp_py"

    # __init__.py files for new directories
    for d in [
        "services/payment",
        "services/notification",
        "middleware",
        "database",
        "api",
        "api/v1",
        "api/v2",
        "tasks",
        "validators",
        "cache",
        "events",
    ]:
        p = f"{prefix}/{d}/__init__.py"
        if not os.path.exists(os.path.join(BASE, p)):
            write(p, f'"""Package {d}."""\n')

    # ── Payment service ──
    write(f"{prefix}/services/payment/__init__.py", '"""Payment services."""\n')
    write(
        f"{prefix}/services/payment/processor.py",
        '''\
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
    ''',
    )

    write(
        f"{prefix}/services/payment/gateway.py",
        '''\
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
    ''',
    )

    # ── Notification service ──
    write(
        f"{prefix}/services/notification/__init__.py", '"""Notification services."""\n'
    )
    write(
        f"{prefix}/services/notification/manager.py",
        '''\
        """Notification management service."""

        import time
        from typing import Any, Dict, List, Optional

        from ...utils.logging import get_logger
        from ...utils.helpers import validate_request, sanitize_input
        from ...database.connection import DatabaseConnection
        from ...exceptions import ValidationError, NotFoundError
        from ..base import BaseService

        _logger = get_logger("services.notification.manager")


        class NotificationChannel:
            """Represents a notification delivery channel."""
            EMAIL = "email"
            SMS = "sms"
            PUSH = "push"
            IN_APP = "in_app"

            ALL = [EMAIL, SMS, PUSH, IN_APP]


        class Notification:
            """A notification to be delivered to a user."""

            def __init__(self, user_id: str, channel: str, subject: str, body: str):
                self.user_id = user_id
                self.channel = channel
                self.subject = subject
                self.body = body
                self.created_at = time.time()
                self.sent_at: Optional[float] = None
                self.status = "pending"

            def mark_sent(self) -> None:
                """Mark notification as successfully sent."""
                self.sent_at = time.time()
                self.status = "sent"

            def mark_failed(self, reason: str) -> None:
                """Mark notification as failed."""
                self.status = f"failed: {reason}"

            def to_dict(self) -> Dict[str, Any]:
                """Serialize notification."""
                return {
                    "user_id": self.user_id,
                    "channel": self.channel,
                    "subject": self.subject,
                    "body": self.body,
                    "status": self.status,
                    "created_at": self.created_at,
                    "sent_at": self.sent_at,
                }


        class NotificationManager(BaseService):
            """Manages notification creation, delivery, and tracking."""

            def __init__(self, db: DatabaseConnection):
                super().__init__(db, "notification_manager")
                self._queue: List[Notification] = []
                self._preferences: Dict[str, List[str]] = {}

            def send(self, user_id: str, channel: str, subject: str, body: str) -> Notification:
                """Create and queue a notification."""
                self._require_initialized()
                _logger.info(f"Queuing notification for {user_id} via {channel}")

                if channel not in NotificationChannel.ALL:
                    raise ValidationError(f"Invalid channel: {channel}", field="channel")

                clean_subject = sanitize_input(subject)
                clean_body = sanitize_input(body)

                notification = Notification(user_id, channel, clean_subject, clean_body)
                self._queue.append(notification)

                # Persist to database
                self._db.insert("notifications", notification.to_dict())

                return notification

            def send_multi_channel(self, user_id: str, subject: str, body: str,
                                   channels: Optional[List[str]] = None) -> List[Notification]:
                """Send notification across multiple channels."""
                target_channels = channels or self._get_user_preferences(user_id)
                notifications = []

                for channel in target_channels:
                    try:
                        n = self.send(user_id, channel, subject, body)
                        notifications.append(n)
                    except Exception as e:
                        _logger.info(f"Failed to send via {channel}: {e}")

                return notifications

            def process_queue(self) -> Dict[str, int]:
                """Process all pending notifications in the queue."""
                _logger.info(f"Processing {len(self._queue)} notifications")
                sent = 0
                failed = 0

                for notification in self._queue:
                    if notification.status == "pending":
                        try:
                            self._deliver(notification)
                            notification.mark_sent()
                            sent += 1
                        except Exception as e:
                            notification.mark_failed(str(e))
                            failed += 1

                self._queue = [n for n in self._queue if n.status == "pending"]
                return {"sent": sent, "failed": failed, "remaining": len(self._queue)}

            def set_preferences(self, user_id: str, channels: List[str]) -> None:
                """Set notification channel preferences for a user."""
                valid = [c for c in channels if c in NotificationChannel.ALL]
                self._preferences[user_id] = valid
                _logger.info(f"Preferences set for {user_id}: {valid}")

            def get_history(self, user_id: str, limit: int = 50) -> List[Dict[str, Any]]:
                """Get notification history for a user."""
                result = self._db.find_all("notifications", {"user_id": user_id}, limit=limit)
                return result

            def _get_user_preferences(self, user_id: str) -> List[str]:
                """Get user's preferred notification channels."""
                return self._preferences.get(user_id, [NotificationChannel.EMAIL])

            def _deliver(self, notification: Notification) -> None:
                """Deliver a notification via its channel."""
                _logger.info(f"Delivering {notification.channel} notification to {notification.user_id}")
                # Actual delivery would happen here
    ''',
    )

    # ── Events ──
    write(f"{prefix}/events/__init__.py", '"""Event system."""\n')
    write(
        f"{prefix}/events/dispatcher.py",
        '''\
        """Event dispatcher for decoupled service communication."""

        import time
        from typing import Any, Callable, Dict, List, Optional

        from ..utils.logging import get_logger

        _logger = get_logger("events.dispatcher")


        class Event:
            """An application event."""

            def __init__(self, event_type: str, data: Dict[str, Any]):
                self.event_type = event_type
                self.data = data
                self.timestamp = time.time()
                self.processed = False

            def to_dict(self) -> Dict[str, Any]:
                """Serialize the event."""
                return {
                    "type": self.event_type,
                    "data": self.data,
                    "timestamp": self.timestamp,
                    "processed": self.processed,
                }


        EventHandler = Callable[[Event], None]


        class EventDispatcher:
            """Central event bus for the application.

            Services emit events, and registered handlers react to them.
            """

            def __init__(self):
                self._handlers: Dict[str, List[EventHandler]] = {}
                self._event_log: List[Event] = []
                self._max_log_size = 1000

            def on(self, event_type: str, handler: EventHandler) -> None:
                """Register a handler for an event type."""
                if event_type not in self._handlers:
                    self._handlers[event_type] = []
                self._handlers[event_type].append(handler)
                _logger.info(f"Handler registered for: {event_type}")

            def off(self, event_type: str, handler: EventHandler) -> None:
                """Unregister a handler."""
                if event_type in self._handlers:
                    self._handlers[event_type] = [
                        h for h in self._handlers[event_type] if h != handler
                    ]

            def emit(self, event_type: str, data: Optional[Dict[str, Any]] = None) -> int:
                """Emit an event and invoke all registered handlers."""
                event = Event(event_type, data or {})
                self._event_log.append(event)

                # Trim log if needed
                if len(self._event_log) > self._max_log_size:
                    self._event_log = self._event_log[-self._max_log_size:]

                handlers = self._handlers.get(event_type, [])
                _logger.info(f"Emitting {event_type} to {len(handlers)} handlers")

                invoked = 0
                for handler in handlers:
                    try:
                        handler(event)
                        invoked += 1
                    except Exception as e:
                        _logger.info(f"Handler error for {event_type}: {e}")

                event.processed = True
                return invoked

            def event_count(self) -> int:
                """Return total events emitted."""
                return len(self._event_log)

            def handler_count(self, event_type: Optional[str] = None) -> int:
                """Return number of registered handlers."""
                if event_type:
                    return len(self._handlers.get(event_type, []))
                return sum(len(h) for h in self._handlers.values())
    ''',
    )

    write(
        f"{prefix}/events/handlers.py",
        '''\
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
    ''',
    )

    # ── Cache ──
    write(f"{prefix}/cache/__init__.py", '"""Caching layer."""\n')
    write(
        f"{prefix}/cache/base.py",
        '''\
        """Base cache interface."""

        from typing import Any, Dict, Optional

        from ..utils.logging import get_logger

        _logger = get_logger("cache.base")


        class BaseCache:
            """Abstract base class for cache implementations."""

            def __init__(self, name: str = "base"):
                self._name = name
                self._hits = 0
                self._misses = 0

            def get(self, key: str) -> Optional[Any]:
                """Retrieve a value by key. Returns None on miss."""
                raise NotImplementedError

            def set(self, key: str, value: Any, ttl: int = 300) -> None:
                """Store a value with optional TTL in seconds."""
                raise NotImplementedError

            def delete(self, key: str) -> bool:
                """Remove a key. Returns True if existed."""
                raise NotImplementedError

            def clear(self) -> int:
                """Remove all entries. Returns count removed."""
                raise NotImplementedError

            def exists(self, key: str) -> bool:
                """Check if a key exists and is not expired."""
                return self.get(key) is not None

            def stats(self) -> Dict[str, Any]:
                """Return hit/miss statistics."""
                total = self._hits + self._misses
                rate = (self._hits / total * 100) if total > 0 else 0
                return {
                    "backend": self._name,
                    "hits": self._hits,
                    "misses": self._misses,
                    "hit_rate": f"{rate:.1f}%",
                }
    ''',
    )

    write(
        f"{prefix}/cache/redis_cache.py",
        '''\
        """Redis-backed cache implementation."""

        import time
        from typing import Any, Dict, Optional

        from ..utils.logging import get_logger
        from .base import BaseCache

        _logger = get_logger("cache.redis")


        class RedisCache(BaseCache):
            """Cache implementation using Redis as backend."""

            def __init__(self, host: str = "localhost", port: int = 6379, db: int = 0):
                super().__init__("redis")
                self._host = host
                self._port = port
                self._db_index = db
                self._store: Dict[str, Any] = {}
                self._expiry: Dict[str, float] = {}
                _logger.info(f"RedisCache created: {host}:{port}/{db}")

            def get(self, key: str) -> Optional[Any]:
                """Get value from Redis."""
                if key in self._store:
                    if key in self._expiry and time.time() > self._expiry[key]:
                        del self._store[key]
                        del self._expiry[key]
                        self._misses += 1
                        return None
                    self._hits += 1
                    return self._store[key]
                self._misses += 1
                return None

            def set(self, key: str, value: Any, ttl: int = 300) -> None:
                """Set value in Redis with TTL."""
                self._store[key] = value
                self._expiry[key] = time.time() + ttl
                _logger.info(f"Redis SET {key} (ttl={ttl})")

            def delete(self, key: str) -> bool:
                """Delete a key from Redis."""
                if key in self._store:
                    del self._store[key]
                    self._expiry.pop(key, None)
                    return True
                return False

            def clear(self) -> int:
                """Flush all keys."""
                count = len(self._store)
                self._store.clear()
                self._expiry.clear()
                _logger.info(f"Redis FLUSHDB: {count} keys removed")
                return count

            def incr(self, key: str, amount: int = 1) -> int:
                """Increment a counter."""
                current = self._store.get(key, 0)
                new_val = current + amount
                self._store[key] = new_val
                return new_val

            def expire(self, key: str, ttl: int) -> bool:
                """Set expiry on an existing key."""
                if key in self._store:
                    self._expiry[key] = time.time() + ttl
                    return True
                return False
    ''',
    )

    write(
        f"{prefix}/cache/memory_cache.py",
        '''\
        """In-memory LRU cache implementation."""

        import time
        from typing import Any, Dict, List, Optional, Tuple
        from collections import OrderedDict

        from ..utils.logging import get_logger
        from .base import BaseCache

        _logger = get_logger("cache.memory")


        class MemoryCache(BaseCache):
            """In-memory cache with LRU eviction."""

            def __init__(self, max_size: int = 1000):
                super().__init__("memory")
                self._max_size = max_size
                self._store: OrderedDict = OrderedDict()
                self._expiry: Dict[str, float] = {}
                _logger.info(f"MemoryCache created: max_size={max_size}")

            def get(self, key: str) -> Optional[Any]:
                """Get value with LRU tracking."""
                if key in self._store:
                    if key in self._expiry and time.time() > self._expiry[key]:
                        del self._store[key]
                        del self._expiry[key]
                        self._misses += 1
                        return None
                    # Move to end (most recently used)
                    self._store.move_to_end(key)
                    self._hits += 1
                    return self._store[key]
                self._misses += 1
                return None

            def set(self, key: str, value: Any, ttl: int = 300) -> None:
                """Set value with LRU eviction."""
                if key in self._store:
                    self._store.move_to_end(key)
                elif len(self._store) >= self._max_size:
                    evicted_key, _ = self._store.popitem(last=False)
                    self._expiry.pop(evicted_key, None)
                    _logger.info(f"LRU evicted: {evicted_key}")

                self._store[key] = value
                self._expiry[key] = time.time() + ttl

            def delete(self, key: str) -> bool:
                """Remove a key."""
                if key in self._store:
                    del self._store[key]
                    self._expiry.pop(key, None)
                    return True
                return False

            def clear(self) -> int:
                """Clear all entries."""
                count = len(self._store)
                self._store.clear()
                self._expiry.clear()
                return count

            def size(self) -> int:
                """Return current number of entries."""
                return len(self._store)

            def keys(self) -> List[str]:
                """Return all keys in LRU order."""
                return list(self._store.keys())
    ''',
    )

    # ── Validators (name collision: validate in 4 files) ──
    write(f"{prefix}/validators/__init__.py", '"""Input validators."""\n')
    write(
        f"{prefix}/validators/common.py",
        '''\
        """Common validation utilities shared across validators."""

        import re
        from typing import Any, Dict, List, Optional

        from ..utils.logging import get_logger
        from ..exceptions import ValidationError

        _logger = get_logger("validators.common")

        EMAIL_REGEX = re.compile(r'^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}$')
        URL_REGEX = re.compile(r'^https?://[\\w.-]+(?:\\.[\\w.-]+)+[\\w.,@?^=%&:/~+#-]*$')


        def validate_email(email: str) -> str:
            """Validate and normalize an email address."""
            if not email or not isinstance(email, str):
                raise ValidationError("Email is required", field="email")

            clean = email.strip().lower()
            if not EMAIL_REGEX.match(clean):
                raise ValidationError(f"Invalid email format: {email}", field="email")

            return clean


        def validate_string(value: str, field: str, min_len: int = 1, max_len: int = 255) -> str:
            """Validate a string field for length constraints."""
            if not value or not isinstance(value, str):
                raise ValidationError(f"{field} is required", field=field)

            stripped = value.strip()
            if len(stripped) < min_len:
                raise ValidationError(f"{field} must be at least {min_len} characters", field=field)
            if len(stripped) > max_len:
                raise ValidationError(f"{field} must be at most {max_len} characters", field=field)

            return stripped


        def validate_positive_number(value: Any, field: str) -> float:
            """Validate that a value is a positive number."""
            try:
                num = float(value)
            except (TypeError, ValueError):
                raise ValidationError(f"{field} must be a number", field=field)

            if num <= 0:
                raise ValidationError(f"{field} must be positive", field=field)

            return num


        def validate_enum(value: str, allowed: List[str], field: str) -> str:
            """Validate that a value is in an allowed set."""
            if value not in allowed:
                raise ValidationError(
                    f"Invalid {field}: '{value}'. Allowed: {', '.join(allowed)}",
                    field=field,
                )
            return value


        def validate_dict_keys(data: Dict[str, Any], required: List[str], optional: Optional[List[str]] = None) -> None:
            """Validate that a dictionary contains required keys."""
            for key in required:
                if key not in data:
                    raise ValidationError(f"Missing required field: {key}", field=key)

            allowed = set(required + (optional or []))
            for key in data:
                if key not in allowed:
                    _logger.info(f"Unknown field ignored: {key}")
    ''',
    )

    write(
        f"{prefix}/validators/user.py",
        '''\
        """User input validation."""

        from typing import Any, Dict

        from ..utils.logging import get_logger
        from ..exceptions import ValidationError
        from .common import validate_email, validate_string, validate_dict_keys

        _logger = get_logger("validators.user")

        PASSWORD_MIN_LENGTH = 8
        PASSWORD_MAX_LENGTH = 128
        NAME_MAX_LENGTH = 100


        def validate(data: Dict[str, Any]) -> Dict[str, Any]:
            """Validate user registration/update data.

            This is the user-specific validate function (name collision with
            validators.payment.validate, api.v1.auth.validate, api.v2.auth.validate).
            """
            _logger.info("Validating user data")
            validate_dict_keys(data, required=["email", "name"], optional=["password", "role"])

            result = {}
            result["email"] = validate_email(data["email"])
            result["name"] = validate_string(data["name"], "name", max_len=NAME_MAX_LENGTH)

            if "password" in data:
                result["password"] = _validate_password(data["password"])

            if "role" in data:
                allowed_roles = ["user", "admin", "moderator"]
                if data["role"] not in allowed_roles:
                    raise ValidationError(f"Invalid role: {data['role']}", field="role")
                result["role"] = data["role"]

            return result


        def validate_login(data: Dict[str, Any]) -> Dict[str, Any]:
            """Validate login request data."""
            validate_dict_keys(data, required=["email", "password"])
            return {
                "email": validate_email(data["email"]),
                "password": data["password"],
            }


        def _validate_password(password: str) -> str:
            """Validate password strength."""
            if len(password) < PASSWORD_MIN_LENGTH:
                raise ValidationError(
                    f"Password must be at least {PASSWORD_MIN_LENGTH} characters",
                    field="password",
                )
            if len(password) > PASSWORD_MAX_LENGTH:
                raise ValidationError("Password too long", field="password")

            has_upper = any(c.isupper() for c in password)
            has_lower = any(c.islower() for c in password)
            has_digit = any(c.isdigit() for c in password)

            if not (has_upper and has_lower and has_digit):
                raise ValidationError(
                    "Password must contain uppercase, lowercase, and digit",
                    field="password",
                )

            return password
    ''',
    )

    write(
        f"{prefix}/validators/payment.py",
        '''\
        """Payment input validation."""

        from typing import Any, Dict

        from ..utils.logging import get_logger
        from ..exceptions import ValidationError
        from .common import validate_positive_number, validate_enum, validate_dict_keys

        _logger = get_logger("validators.payment")

        SUPPORTED_CURRENCIES = ["USD", "EUR", "GBP", "JPY", "CAD"]
        PAYMENT_METHODS = ["card", "bank_transfer", "wallet"]


        def validate(data: Dict[str, Any]) -> Dict[str, Any]:
            """Validate payment request data.

            This is the payment-specific validate function (name collision with
            validators.user.validate, api.v1.auth.validate, api.v2.auth.validate).
            """
            _logger.info("Validating payment data")
            validate_dict_keys(data, required=["amount", "currency", "user_id"],
                              optional=["payment_method", "description"])

            result = {}
            result["amount"] = validate_positive_number(data["amount"], "amount")
            result["currency"] = validate_enum(data["currency"], SUPPORTED_CURRENCIES, "currency")
            result["user_id"] = data["user_id"]

            if "payment_method" in data:
                result["payment_method"] = validate_enum(
                    data["payment_method"], PAYMENT_METHODS, "payment_method"
                )
            else:
                result["payment_method"] = "card"

            # Validate amount ranges per currency
            max_amounts = {"USD": 999999, "EUR": 999999, "GBP": 999999, "JPY": 99999999, "CAD": 999999}
            max_amount = max_amounts.get(result["currency"], 999999)
            if result["amount"] > max_amount:
                raise ValidationError(
                    f"Amount exceeds maximum for {result['currency']}: {max_amount}",
                    field="amount",
                )

            return result


        def validate_refund(data: Dict[str, Any]) -> Dict[str, Any]:
            """Validate refund request data."""
            validate_dict_keys(data, required=["transaction_id"], optional=["reason", "amount"])

            result = {"transaction_id": data["transaction_id"]}

            if "amount" in data:
                result["amount"] = validate_positive_number(data["amount"], "amount")

            if "reason" in data:
                result["reason"] = str(data["reason"])[:500]

            return result
    ''',
    )

    # ── Middleware ──
    write(f"{prefix}/middleware/__init__.py", '"""HTTP middleware."""\n')
    write(
        f"{prefix}/middleware/rate_limit.py",
        '''\
        """Rate limiting middleware."""

        import time
        from typing import Any, Dict, Optional

        from ..utils.logging import get_logger
        from ..utils.helpers import validate_request
        from ..exceptions import RateLimitError
        from ..cache.base import BaseCache

        _logger = get_logger("middleware.rate_limit")

        DEFAULT_LIMIT = 100
        DEFAULT_WINDOW = 60


        class RateLimiter:
            """Token-bucket rate limiter backed by cache."""

            def __init__(self, cache: BaseCache, limit: int = DEFAULT_LIMIT,
                         window: int = DEFAULT_WINDOW):
                self._cache = cache
                self._limit = limit
                self._window = window

            def check(self, key: str) -> Dict[str, Any]:
                """Check if a request is within rate limits."""
                cache_key = f"ratelimit:{key}"
                current = self._cache.get(cache_key)

                if current is None:
                    self._cache.set(cache_key, 1, ttl=self._window)
                    return {"allowed": True, "remaining": self._limit - 1, "limit": self._limit}

                count = int(current)
                if count >= self._limit:
                    _logger.info(f"Rate limit exceeded for {key}")
                    return {"allowed": False, "remaining": 0, "limit": self._limit}

                self._cache.set(cache_key, count + 1, ttl=self._window)
                return {"allowed": True, "remaining": self._limit - count - 1, "limit": self._limit}


        def rate_limit_middleware(request: Dict[str, Any], cache: BaseCache,
                                limit: int = DEFAULT_LIMIT) -> Dict[str, Any]:
            """Apply rate limiting to a request."""
            validate_request(request)
            ip = request.get("ip", "unknown")
            path = request.get("path", "/")
            key = f"{ip}:{path}"

            limiter = RateLimiter(cache, limit=limit)
            result = limiter.check(key)

            if not result["allowed"]:
                raise RateLimitError(retry_after=DEFAULT_WINDOW)

            request["rate_limit"] = result
            return request
    ''',
    )

    write(
        f"{prefix}/middleware/cors.py",
        '''\
        """CORS middleware for cross-origin request handling."""

        from typing import Any, Dict, List, Optional

        from ..utils.logging import get_logger
        from ..utils.helpers import validate_request

        _logger = get_logger("middleware.cors")

        DEFAULT_ALLOWED_ORIGINS = ["http://localhost:3000", "https://app.example.com"]
        DEFAULT_ALLOWED_METHODS = ["GET", "POST", "PUT", "DELETE", "OPTIONS"]
        DEFAULT_ALLOWED_HEADERS = ["Content-Type", "Authorization", "X-Request-ID"]


        class CorsPolicy:
            """CORS policy configuration."""

            def __init__(self, allowed_origins: Optional[List[str]] = None,
                         allowed_methods: Optional[List[str]] = None,
                         allowed_headers: Optional[List[str]] = None,
                         allow_credentials: bool = True,
                         max_age: int = 86400):
                self.allowed_origins = allowed_origins or DEFAULT_ALLOWED_ORIGINS
                self.allowed_methods = allowed_methods or DEFAULT_ALLOWED_METHODS
                self.allowed_headers = allowed_headers or DEFAULT_ALLOWED_HEADERS
                self.allow_credentials = allow_credentials
                self.max_age = max_age

            def is_origin_allowed(self, origin: str) -> bool:
                """Check if an origin is allowed."""
                if "*" in self.allowed_origins:
                    return True
                return origin in self.allowed_origins

            def get_headers(self, origin: str) -> Dict[str, str]:
                """Generate CORS response headers."""
                if not self.is_origin_allowed(origin):
                    return {}

                headers = {
                    "Access-Control-Allow-Origin": origin,
                    "Access-Control-Allow-Methods": ", ".join(self.allowed_methods),
                    "Access-Control-Allow-Headers": ", ".join(self.allowed_headers),
                    "Access-Control-Max-Age": str(self.max_age),
                }

                if self.allow_credentials:
                    headers["Access-Control-Allow-Credentials"] = "true"

                return headers


        def cors_middleware(request: Dict[str, Any], policy: Optional[CorsPolicy] = None) -> Dict[str, Any]:
            """Apply CORS headers to a request/response cycle."""
            validate_request(request)
            cors = policy or CorsPolicy()

            origin = request.get("origin", "")
            if origin:
                headers = cors.get_headers(origin)
                request["cors_headers"] = headers
                if not headers:
                    _logger.info(f"CORS rejected origin: {origin}")
            else:
                request["cors_headers"] = {}

            return request
    ''',
    )

    write(
        f"{prefix}/middleware/logging_mw.py",
        '''\
        """Request/response logging middleware."""

        import time
        from typing import Any, Dict

        from ..utils.logging import get_logger
        from ..utils.helpers import validate_request, generate_request_id, mask_sensitive

        _logger = get_logger("middleware.logging")

        SENSITIVE_FIELDS = ["password", "token", "secret", "api_key", "authorization"]


        def logging_middleware(request: Dict[str, Any]) -> Dict[str, Any]:
            """Log incoming requests with timing and request ID."""
            validate_request(request)

            # Assign request ID
            request_id = request.get("request_id") or generate_request_id()
            request["request_id"] = request_id

            # Log sanitized request
            safe_request = mask_sensitive(request, SENSITIVE_FIELDS)
            method = request.get("method", "?")
            path = request.get("path", "?")
            _logger.info(f"[{request_id}] {method} {path}")

            # Record timing
            request["_start_time"] = time.time()
            return request


        def log_response(request: Dict[str, Any], status: int, body_size: int = 0) -> None:
            """Log response details with timing."""
            request_id = request.get("request_id", "unknown")
            start = request.get("_start_time", time.time())
            duration = (time.time() - start) * 1000  # milliseconds

            method = request.get("method", "?")
            path = request.get("path", "?")
            _logger.info(f"[{request_id}] {method} {path} -> {status} ({duration:.1f}ms, {body_size}B)")
    ''',
    )

    write(
        f"{prefix}/middleware/auth_mw.py",
        '''\
        """Authentication middleware."""

        from typing import Any, Dict, Optional

        from ..utils.logging import get_logger
        from ..utils.helpers import validate_request
        from ..auth.tokens import validate_token
        from ..exceptions import AuthenticationError, AuthorizationError

        _logger = get_logger("middleware.auth")

        PUBLIC_PATHS = ["/health", "/login", "/register", "/docs"]


        def auth_middleware(request: Dict[str, Any]) -> Dict[str, Any]:
            """Verify authentication token and attach user context."""
            validate_request(request)

            path = request.get("path", "")
            if path in PUBLIC_PATHS:
                return request

            token = _extract_token(request)
            if not token:
                raise AuthenticationError("Missing authentication token")

            try:
                claims = validate_token(token)
            except Exception as e:
                _logger.info(f"Token validation failed: {e}")
                raise AuthenticationError("Invalid or expired token")

            request["user"] = claims
            request["authenticated"] = True
            _logger.info(f"Authenticated: user={claims.get('user_id', 'unknown')}")

            return request


        def require_role(request: Dict[str, Any], required_role: str) -> None:
            """Verify the authenticated user has the required role."""
            user = request.get("user", {})
            user_role = user.get("role", "user")

            role_hierarchy = {"admin": 3, "moderator": 2, "user": 1}
            if role_hierarchy.get(user_role, 0) < role_hierarchy.get(required_role, 0):
                raise AuthorizationError(required_role, request.get("path", "unknown"))


        def _extract_token(request: Dict[str, Any]) -> Optional[str]:
            """Extract bearer token from request headers."""
            auth_header = request.get("headers", {}).get("Authorization", "")
            if auth_header.startswith("Bearer "):
                return auth_header[7:]
            return request.get("token")
    ''',
    )

    # ── API versions ──
    write(f"{prefix}/api/__init__.py", '"""API versioned endpoints."""\n')
    write(f"{prefix}/api/v1/__init__.py", '"""API v1."""\n')
    write(
        f"{prefix}/api/v1/auth.py",
        '''\
        """API v1 authentication endpoints."""

        from typing import Any, Dict

        from ...utils.logging import get_logger
        from ...utils.helpers import validate_request, sanitize_input
        from ...validators.user import validate_login
        from ...services.auth_service import AuthenticationService
        from ...database.connection import DatabaseConnection
        from ...events.dispatcher import EventDispatcher
        from ...exceptions import AuthenticationError, ValidationError

        _logger = get_logger("api.v1.auth")


        def validate(request: Dict[str, Any]) -> Dict[str, Any]:
            """Validate an API v1 auth request.

            Name collision: same function name as validators.user.validate,
            validators.payment.validate, and api.v2.auth.validate.
            """
            validate_request(request)
            body = request.get("body", {})

            if not body:
                raise ValidationError("Request body is required")

            # V1 requires 'username' field (legacy)
            if "username" in body and "email" not in body:
                body["email"] = body["username"]

            return body


        def handle_login(request: Dict[str, Any], db: DatabaseConnection,
                         events: EventDispatcher) -> Dict[str, Any]:
            """Handle v1 login request — entry point for deep call chain.

            Call chain: handle_login -> authenticate -> login -> generate_token
                        -> execute_query -> get_connection
            """
            _logger.info("API v1 login request")
            body = validate(request)
            login_data = validate_login(body)

            service = AuthenticationService(db, events)
            service.initialize()

            ip = request.get("ip", "unknown")
            result = service.authenticate(login_data["email"], login_data["password"], ip)

            _logger.info(f"Login successful: {login_data['email']}")
            return {"status": 200, "data": result}


        def handle_register(request: Dict[str, Any], db: DatabaseConnection,
                            events: EventDispatcher) -> Dict[str, Any]:
            """Handle v1 registration request."""
            _logger.info("API v1 register request")
            body = validate(request)

            service = AuthenticationService(db, events)
            service.initialize()

            result = service.register(
                email=sanitize_input(body.get("email", "")),
                password=body.get("password", ""),
                name=sanitize_input(body.get("name", "")),
            )

            return {"status": 201, "data": result}


        def handle_logout(request: Dict[str, Any], db: DatabaseConnection,
                          events: EventDispatcher) -> Dict[str, Any]:
            """Handle v1 logout request."""
            _logger.info("API v1 logout request")
            token = request.get("token", "")

            service = AuthenticationService(db, events)
            service.initialize()
            service.logout(token)

            return {"status": 200, "data": {"message": "Logged out"}}
    ''',
    )

    write(
        f"{prefix}/api/v1/payments.py",
        '''\
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
    ''',
    )

    write(
        f"{prefix}/api/v1/users.py",
        '''\
        """API v1 user management endpoints."""

        from typing import Any, Dict, List

        from ...utils.logging import get_logger
        from ...utils.helpers import validate_request, paginate
        from ...validators.user import validate as validate_user_data
        from ...database.connection import DatabaseConnection
        from ...database.queries import UserQueries
        from ...exceptions import NotFoundError, AuthorizationError

        _logger = get_logger("api.v1.users")


        def handle_get_user(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
            """Get a user by ID."""
            validate_request(request)
            user_id = request.get("params", {}).get("id", "")
            _logger.info(f"Getting user: {user_id}")

            queries = UserQueries(db)
            user = db.find_by_id("users", user_id)

            if not user:
                raise NotFoundError("User", user_id)

            return {"status": 200, "data": user}


        def handle_update_user(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
            """Update user profile."""
            validate_request(request)
            user_id = request.get("params", {}).get("id", "")
            body = request.get("body", {})
            _logger.info(f"Updating user: {user_id}")

            # Validate and sanitize
            validated = validate_user_data(body)
            db.update("users", user_id, validated)

            return {"status": 200, "data": {"id": user_id, **validated}}


        def handle_list_users(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
            """List users with pagination."""
            validate_request(request)
            page = int(request.get("params", {}).get("page", 1))
            per_page = int(request.get("params", {}).get("per_page", 20))
            _logger.info(f"Listing users: page={page}")

            queries = UserQueries(db)
            users = queries.find_active_users(limit=per_page * 10)
            result = paginate(users, page=page, per_page=per_page)

            return {"status": 200, "data": result}


        def handle_search_users(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
            """Search users by name or email."""
            validate_request(request)
            query = request.get("params", {}).get("q", "")
            _logger.info(f"Searching users: q={query}")

            queries = UserQueries(db)
            result = queries.search_users(query)

            return {"status": 200, "data": result.rows}
    ''',
    )

    write(f"{prefix}/api/v2/__init__.py", '"""API v2."""\n')
    write(
        f"{prefix}/api/v2/auth.py",
        '''\
        """API v2 authentication endpoints — improved over v1."""

        from typing import Any, Dict

        from ...utils.logging import get_logger
        from ...utils.helpers import validate_request, sanitize_input
        from ...validators.user import validate_login
        from ...services.auth_service import AuthenticationService
        from ...database.connection import DatabaseConnection
        from ...events.dispatcher import EventDispatcher
        from ...exceptions import AuthenticationError, ValidationError, RateLimitError

        _logger = get_logger("api.v2.auth")


        def validate(request: Dict[str, Any]) -> Dict[str, Any]:
            """Validate an API v2 auth request.

            Name collision: same function name as validators.user.validate,
            validators.payment.validate, and api.v1.auth.validate.
            V2 adds stricter validation and rate limit awareness.
            """
            validate_request(request)
            body = request.get("body", {})

            if not body:
                raise ValidationError("Request body is required")

            # V2 requires 'email' (no legacy 'username' support)
            if "email" not in body:
                raise ValidationError("Email is required", field="email")

            # V2 requires content-type header
            content_type = request.get("headers", {}).get("Content-Type", "")
            if "json" not in content_type.lower():
                _logger.info(f"Invalid content type: {content_type}")

            return body


        def handle_login(request: Dict[str, Any], db: DatabaseConnection,
                         events: EventDispatcher) -> Dict[str, Any]:
            """Handle v2 login — adds rate limiting and device tracking."""
            _logger.info("API v2 login request")
            body = validate(request)
            login_data = validate_login(body)

            service = AuthenticationService(db, events)
            service.initialize()

            ip = request.get("ip", "unknown")
            user_agent = request.get("headers", {}).get("User-Agent", "")

            result = service.authenticate(login_data["email"], login_data["password"], ip)
            result["api_version"] = "v2"
            result["device"] = user_agent[:100]

            _logger.info(f"V2 login successful: {login_data['email']}")
            return {"status": 200, "data": result}


        def handle_token_refresh(request: Dict[str, Any], db: DatabaseConnection,
                                 events: EventDispatcher) -> Dict[str, Any]:
            """Handle v2 token refresh — v1 doesn't have this."""
            _logger.info("API v2 token refresh")
            validate_request(request)

            old_token = request.get("token", "")
            if not old_token:
                raise AuthenticationError("Refresh token required")

            service = AuthenticationService(db, events)
            service.initialize()

            user = service.verify_token(old_token)
            if not user:
                raise AuthenticationError("Invalid refresh token")

            # Generate new token pair
            from ...auth.tokens import generate_token
            new_token = generate_token(user)

            return {
                "status": 200,
                "data": {
                    "token": new_token,
                    "api_version": "v2",
                },
            }
    ''',
    )

    write(
        f"{prefix}/api/v2/payments.py",
        '''\
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
    ''',
    )

    # ── Tasks ──
    write(f"{prefix}/tasks/__init__.py", '"""Background tasks."""\n')
    write(
        f"{prefix}/tasks/email_task.py",
        '''\
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
    ''',
    )

    write(
        f"{prefix}/tasks/payment_task.py",
        '''\
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
    ''',
    )

    write(
        f"{prefix}/tasks/cleanup_task.py",
        '''\
        """Background task for cleanup and maintenance."""

        import time
        from typing import Any, Dict

        from ..utils.logging import get_logger
        from ..database.connection import DatabaseConnection
        from ..database.pool import ConnectionPool
        from ..database.queries import SessionQueries
        from ..cache.base import BaseCache

        _logger = get_logger("tasks.cleanup")

        SESSION_MAX_AGE = 86400 * 7  # 7 days
        EVENT_MAX_AGE = 86400 * 30   # 30 days


        def cleanup_expired_sessions(db: DatabaseConnection) -> int:
            """Remove sessions older than SESSION_MAX_AGE."""
            _logger.info("Cleaning up expired sessions")

            sessions = SessionQueries(db)
            cutoff = time.time() - SESSION_MAX_AGE

            result = db.execute_query(
                "UPDATE sessions SET expired_at = ? WHERE expired_at IS NULL AND created_at < ?",
                (time.strftime("%Y-%m-%dT%H:%M:%SZ"), str(cutoff)),
            )

            _logger.info(f"Expired {result.affected} stale sessions")
            return result.affected


        def cleanup_old_events(db: DatabaseConnection) -> int:
            """Remove processed events older than EVENT_MAX_AGE."""
            _logger.info("Cleaning up old events")

            cutoff = time.time() - EVENT_MAX_AGE
            result = db.execute_query(
                "DELETE FROM events WHERE processed_at IS NOT NULL AND created_at < ?",
                (str(cutoff),),
            )

            _logger.info(f"Removed {result.affected} old events")
            return result.affected


        def cleanup_cache(cache: BaseCache) -> int:
            """Flush stale cache entries."""
            _logger.info("Running cache cleanup")
            cleared = cache.clear()
            _logger.info(f"Cache cleared: {cleared} entries")
            return cleared


        def run_all_cleanup(db: DatabaseConnection, cache: BaseCache) -> Dict[str, int]:
            """Run all cleanup tasks."""
            _logger.info("Running full cleanup cycle")

            sessions = cleanup_expired_sessions(db)
            events = cleanup_old_events(db)
            cache_entries = cleanup_cache(cache)

            pool_stats = {}
            _logger.info("Cleanup complete")

            return {
                "expired_sessions": sessions,
                "old_events": events,
                "cache_cleared": cache_entries,
            }
    ''',
    )

    # ── Models expansion ──
    write(
        f"{prefix}/models/payment.py",
        '''\
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
    ''',
    )

    write(
        f"{prefix}/models/notification.py",
        '''\
        """Notification model."""

        import time
        from typing import Any, Dict, Optional


        class NotificationRecord:
            """Represents a persisted notification."""

            def __init__(self, user_id: str, channel: str, subject: str,
                         body: str, status: str = "pending"):
                self.user_id = user_id
                self.channel = channel
                self.subject = subject
                self.body = body
                self.status = status
                self.created_at = time.time()
                self.sent_at: Optional[float] = None
                self.read_at: Optional[float] = None

            def mark_sent(self) -> None:
                """Mark as sent."""
                self.status = "sent"
                self.sent_at = time.time()

            def mark_read(self) -> None:
                """Mark as read by user."""
                self.status = "read"
                self.read_at = time.time()

            def to_dict(self) -> Dict[str, Any]:
                """Serialize notification."""
                return {
                    "user_id": self.user_id,
                    "channel": self.channel,
                    "subject": self.subject,
                    "body": self.body,
                    "status": self.status,
                    "created_at": self.created_at,
                    "sent_at": self.sent_at,
                    "read_at": self.read_at,
                }
    ''',
    )

    write(
        f"{prefix}/models/event.py",
        '''\
        """Event model for the event system."""

        import time
        from typing import Any, Dict, Optional


        class EventRecord:
            """Represents a persisted event."""

            def __init__(self, event_type: str, payload: Dict[str, Any]):
                self.event_type = event_type
                self.payload = payload
                self.created_at = time.time()
                self.processed_at: Optional[float] = None

            def mark_processed(self) -> None:
                """Mark event as processed."""
                self.processed_at = time.time()

            def is_processed(self) -> bool:
                """Check if event has been processed."""
                return self.processed_at is not None

            def age_seconds(self) -> float:
                """Return age of event in seconds."""
                return time.time() - self.created_at

            def to_dict(self) -> Dict[str, Any]:
                """Serialize event."""
                return {
                    "type": self.event_type,
                    "payload": self.payload,
                    "created_at": self.created_at,
                    "processed_at": self.processed_at,
                }
    ''',
    )

    # ── Routes expansion ──
    write(
        f"{prefix}/routes/payments.py",
        '''\
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
    ''',
    )

    write(
        f"{prefix}/routes/users.py",
        '''\
        """User management route handlers."""

        from typing import Any, Dict

        from ..utils.logging import get_logger
        from ..utils.helpers import validate_request, paginate
        from ..auth.middleware import auth_required
        from ..database.connection import DatabaseConnection
        from ..database.queries import UserQueries
        from ..validators.user import validate as validate_user_data
        from ..exceptions import NotFoundError

        _logger = get_logger("routes.users")


        def get_user_route(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
            """Get a single user by ID."""
            validate_request(request)
            user_id = request.get("params", {}).get("id", "")
            _logger.info(f"Fetching user {user_id}")

            user = db.find_by_id("users", user_id)
            if not user:
                raise NotFoundError("User", user_id)

            return {"status": 200, "data": user}


        def list_users_route(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
            """List users with pagination."""
            validate_request(request)
            queries = UserQueries(db)
            page = int(request.get("params", {}).get("page", 1))

            users = queries.find_active_users(limit=200)
            result = paginate(users, page=page)

            return {"status": 200, "data": result}


        def update_user_route(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
            """Update user profile."""
            validate_request(request)
            user_id = request.get("params", {}).get("id", "")
            body = request.get("body", {})

            validated = validate_user_data(body)
            db.update("users", user_id, validated)

            _logger.info(f"Updated user {user_id}")
            return {"status": 200, "data": {"id": user_id, **validated}}


        def delete_user_route(request: Dict[str, Any], db: DatabaseConnection) -> Dict[str, Any]:
            """Soft-delete a user."""
            validate_request(request)
            user_id = request.get("params", {}).get("id", "")
            _logger.info(f"Deleting user {user_id}")

            queries = UserQueries(db)
            queries.soft_delete(user_id)

            return {"status": 200, "data": {"deleted": True}}
    ''',
    )

    write(
        f"{prefix}/routes/notifications.py",
        '''\
        """Notification route handlers."""

        from typing import Any, Dict

        from ..utils.logging import get_logger
        from ..utils.helpers import validate_request
        from ..database.connection import DatabaseConnection
        from ..services.notification.manager import NotificationManager

        _logger = get_logger("routes.notifications")


        def send_notification_route(request: Dict[str, Any],
                                     db: DatabaseConnection) -> Dict[str, Any]:
            """Send a notification to a user."""
            validate_request(request)
            body = request.get("body", {})

            manager = NotificationManager(db)
            manager.initialize()

            notification = manager.send(
                user_id=body.get("user_id", ""),
                channel=body.get("channel", "email"),
                subject=body.get("subject", ""),
                body=body.get("body", ""),
            )

            return {"status": 201, "data": notification.to_dict()}


        def list_notifications_route(request: Dict[str, Any],
                                      db: DatabaseConnection) -> Dict[str, Any]:
            """List notifications for the authenticated user."""
            validate_request(request)
            user_id = request.get("user", {}).get("user_id", "")

            manager = NotificationManager(db)
            manager.initialize()

            history = manager.get_history(user_id)
            return {"status": 200, "data": history}
    ''',
    )

    print(f"Python fixture generation complete")


# ============================================================================
# MAIN
# ============================================================================
if __name__ == "__main__":
    gen_python()

    # Count lines
    import subprocess

    for lang in ["py", "rs", "rb", "ts", "go"]:
        d = os.path.join(BASE, f"webapp_{lang}")
        if os.path.isdir(d):
            result = subprocess.run(
                f"find {d} -name '*.{lang}' -o -name '*.py' | head -200 | xargs wc -l 2>/dev/null | tail -1",
                shell=True,
                capture_output=True,
                text=True,
            )
            print(f"  webapp_{lang}: {result.stdout.strip()}")
