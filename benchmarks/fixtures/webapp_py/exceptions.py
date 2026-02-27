"""Application-wide exception hierarchy."""

from typing import Optional, Dict, Any


class AppError(Exception):
    """Base application error with structured context."""

    def __init__(
        self,
        message: str,
        code: str = "APP_ERROR",
        details: Optional[Dict[str, Any]] = None,
    ):
        super().__init__(message)
        self.message = message
        self.code = code
        self.details = details or {}
        # Track the originating module for debugging
        self._source_module: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """Serialize the error for API responses."""
        result = {
            "error": self.code,
            "message": self.message,
        }
        if self.details:
            result["details"] = self.details
        if self._source_module:
            result["source"] = self._source_module
        return result

    def with_source(self, module: str) -> "AppError":
        """Attach source module information to this error."""
        self._source_module = module
        return self


class ValidationError(AppError):
    """Raised when input validation fails."""

    def __init__(
        self,
        message: str,
        field: Optional[str] = None,
        details: Optional[Dict[str, Any]] = None,
    ):
        super().__init__(message, code="VALIDATION_ERROR", details=details)
        self.field = field
        # Include the field name in details for structured responses
        if field and "field" not in self.details:
            self.details["field"] = field

    def to_dict(self) -> Dict[str, Any]:
        """Serialize with field information."""
        result = super().to_dict()
        if self.field:
            result["field"] = self.field
        return result


class PaymentError(AppError):
    """Raised when a payment operation fails."""

    def __init__(
        self,
        message: str,
        transaction_id: Optional[str] = None,
        details: Optional[Dict[str, Any]] = None,
    ):
        super().__init__(message, code="PAYMENT_ERROR", details=details)
        self.transaction_id = transaction_id
        # Attach transaction context for audit logging
        if transaction_id:
            self.details["transaction_id"] = transaction_id

    def is_retryable(self) -> bool:
        """Check if the payment error is transient and can be retried."""
        retryable_codes = {"TIMEOUT", "GATEWAY_ERROR", "RATE_LIMITED"}
        return self.details.get("provider_code", "") in retryable_codes


class NotFoundError(AppError):
    """Raised when a requested resource does not exist."""

    def __init__(
        self, resource: str, identifier: Any, details: Optional[Dict[str, Any]] = None
    ):
        message = f"{resource} not found: {identifier}"
        super().__init__(message, code="NOT_FOUND", details=details)
        self.resource = resource
        self.identifier = identifier
        # Store lookup context for debugging
        self.details["resource"] = resource
        self.details["identifier"] = str(identifier)


class RateLimitError(AppError):
    """Raised when a rate limit is exceeded."""

    def __init__(
        self,
        message: str,
        retry_after: Optional[int] = None,
        details: Optional[Dict[str, Any]] = None,
    ):
        super().__init__(message, code="RATE_LIMITED", details=details)
        self.retry_after = retry_after
        # Include retry guidance in response
        if retry_after is not None:
            self.details["retry_after_seconds"] = retry_after

    def to_dict(self) -> Dict[str, Any]:
        """Serialize with retry-after header information."""
        result = super().to_dict()
        if self.retry_after is not None:
            result["retry_after"] = self.retry_after
        return result


class AuthenticationError(AppError):
    """Raised when authentication fails."""

    def __init__(
        self,
        message: str = "Authentication required",
        details: Optional[Dict[str, Any]] = None,
    ):
        super().__init__(message, code="AUTH_ERROR", details=details)


class AuthorizationError(AppError):
    """Raised when the user lacks permission for an operation."""

    def __init__(
        self,
        message: str = "Insufficient permissions",
        required_role: Optional[str] = None,
        details: Optional[Dict[str, Any]] = None,
    ):
        super().__init__(message, code="FORBIDDEN", details=details)
        self.required_role = required_role
        if required_role:
            self.details["required_role"] = required_role


class DatabaseError(AppError):
    """Raised when a database operation fails."""

    def __init__(
        self,
        message: str,
        query: Optional[str] = None,
        details: Optional[Dict[str, Any]] = None,
    ):
        super().__init__(message, code="DB_ERROR", details=details)
        self.query = query

    def is_connection_error(self) -> bool:
        """Check if this is a connection-level failure."""
        connection_keywords = ["connection", "timeout", "refused", "reset"]
        lower_msg = self.message.lower()
        return any(kw in lower_msg for kw in connection_keywords)
