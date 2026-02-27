package errors

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var log = logger.GetLogger("errors")

// AppError is the base error type for the application.
type AppError struct {
    Message string
    Code    int
    Cause   error
}

// Error implements the error interface.
func (e *AppError) Error() string {
    if e.Cause != nil {
        return fmt.Sprintf("[%d] %s: %v", e.Code, e.Message, e.Cause)
    }
    return fmt.Sprintf("[%d] %s", e.Code, e.Message)
}

// Unwrap returns the underlying cause.
func (e *AppError) Unwrap() error {
    return e.Cause
}

// NewAppError creates a new AppError with the given message and code.
func NewAppError(message string, code int) *AppError {
    log.Error("AppError created: %s (code=%d)", message, code)
    return &AppError{Message: message, Code: code}
}

// ValidationError represents a validation failure.
type ValidationError struct {
    AppError
    Field string
}

// NewValidationError creates a validation error for a specific field.
func NewValidationError(field, message string) *ValidationError {
    log.Warn("Validation failed: field=%s, msg=%s", field, message)
    return &ValidationError{
        AppError: AppError{Message: message, Code: 400},
        Field:    field,
    }
}

// PaymentError represents a payment processing failure.
type PaymentError struct {
    AppError
    TransactionID string
}

// NewPaymentError creates a payment error for a given transaction.
func NewPaymentError(txnID, message string) *PaymentError {
    log.Error("Payment error: txn=%s, msg=%s", txnID, message)
    return &PaymentError{
        AppError:      AppError{Message: message, Code: 402},
        TransactionID: txnID,
    }
}

// NotFoundError represents a resource not found error.
type NotFoundError struct {
    AppError
    Resource string
}

// NewNotFoundError creates a not-found error for a given resource.
func NewNotFoundError(resource, id string) *NotFoundError {
    log.Warn("Not found: resource=%s, id=%s", resource, id)
    return &NotFoundError{
        AppError: AppError{Message: fmt.Sprintf("%s not found: %s", resource, id), Code: 404},
        Resource: resource,
    }
}

// RateLimitError represents a rate limiting error.
type RateLimitError struct {
    AppError
    RetryAfter int
}

// NewRateLimitError creates a rate limit error with retry-after seconds.
func NewRateLimitError(retryAfter int) *RateLimitError {
    log.Warn("Rate limit exceeded, retry after %d seconds", retryAfter)
    return &RateLimitError{
        AppError:   AppError{Message: "rate limit exceeded", Code: 429},
        RetryAfter: retryAfter,
    }
}
