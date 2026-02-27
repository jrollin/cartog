package validators

import (
	"fmt"
	"regexp"
	"strings"

	"webapp_go/pkg/logger"
)

var validLog = logger.GetLogger("validators.common")

// ValidationError represents a field-specific validation error.
type ValidationError struct {
    Field   string
    Message string
}

// Error implements the error interface.
func (e *ValidationError) Error() string {
	return fmt.Sprintf("validation error on %s: %s", e.Field, e.Message)
}

// CommonValidators provides reusable validation functions.
type CommonValidators struct{}

// ValidateEmail checks if an email address is valid.
func (v *CommonValidators) ValidateEmail(email string) *ValidationError {
    validLog.Debug("Validating email: %s", email)
    pattern := regexp.MustCompile(`^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$`)
    if !pattern.MatchString(email) {
        validLog.Warn("Invalid email: %s", email)
        return &ValidationError{Field: "email", Message: "invalid email format"}
    }
    return nil
}

// ValidateRequired checks that a value is not empty.
func (v *CommonValidators) ValidateRequired(field, value string) *ValidationError {
    validLog.Debug("Validating required field: %s", field)
    if strings.TrimSpace(value) == "" {
        validLog.Warn("Required field empty: %s", field)
        return &ValidationError{Field: field, Message: "field is required"}
    }
    return nil
}

// ValidateMinLength checks that a string meets the minimum length.
func (v *CommonValidators) ValidateMinLength(field, value string, min int) *ValidationError {
    validLog.Debug("Validating min length for %s: %d", field, min)
    if len(value) < min {
        validLog.Warn("Field %s too short: %d < %d", field, len(value), min)
        return &ValidationError{Field: field, Message: "value too short"}
    }
    return nil
}

// ValidateMaxLength checks that a string does not exceed the maximum length.
func (v *CommonValidators) ValidateMaxLength(field, value string, max int) *ValidationError {
    validLog.Debug("Validating max length for %s: %d", field, max)
    if len(value) > max {
        validLog.Warn("Field %s too long: %d > %d", field, len(value), max)
        return &ValidationError{Field: field, Message: "value too long"}
    }
    return nil
}

// ValidateRange checks that a number is within a range.
func (v *CommonValidators) ValidateRange(field string, value, min, max float64) *ValidationError {
    validLog.Debug("Validating range for %s: %.2f in [%.2f, %.2f]", field, value, min, max)
    if value < min || value > max {
        validLog.Warn("Field %s out of range: %.2f", field, value)
        return &ValidationError{Field: field, Message: "value out of range"}
    }
    return nil
}
