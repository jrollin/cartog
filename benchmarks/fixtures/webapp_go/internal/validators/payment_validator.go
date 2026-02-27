package validators

import (
    "strconv"

    "webapp_go/pkg/logger"
)

var payValLog = logger.GetLogger("validators.payment")

// PaymentValidator validates payment-related data.
type PaymentValidator struct {
    CommonValidators
}

// NewPaymentValidator creates a new payment validator.
func NewPaymentValidator() *PaymentValidator {
    payValLog.Info("Creating PaymentValidator")
    return &PaymentValidator{}
}

// Validate checks all payment fields.
func (v *PaymentValidator) Validate(data map[string]string) []*ValidationError {
    payValLog.Info("Validating payment data")
    var errs []*ValidationError

    if err := v.ValidateRequired("amount", data["amount"]); err != nil {
        errs = append(errs, err)
    } else {
        amount, parseErr := strconv.ParseFloat(data["amount"], 64)
        if parseErr != nil {
            errs = append(errs, &ValidationError{Field: "amount", Message: "must be a number"})
        } else if err := v.ValidateRange("amount", amount, 0.01, 999999.99); err != nil {
            errs = append(errs, err)
        }
    }

    if err := v.ValidateRequired("currency", data["currency"]); err != nil {
        errs = append(errs, err)
    } else if err := v.ValidateMaxLength("currency", data["currency"], 3); err != nil {
        errs = append(errs, err)
    }

    if err := v.ValidateRequired("user_id", data["user_id"]); err != nil {
        errs = append(errs, err)
    }

    if len(errs) > 0 {
        payValLog.Warn("Payment validation failed: %d errors", len(errs))
    } else {
        payValLog.Info("Payment validation passed")
    }
    return errs
}

// ValidateRefund checks refund-specific fields.
func (v *PaymentValidator) ValidateRefund(data map[string]string) []*ValidationError {
    payValLog.Info("Validating refund data")
    var errs []*ValidationError

    if err := v.ValidateRequired("payment_id", data["payment_id"]); err != nil {
        errs = append(errs, err)
    }
    if err := v.ValidateRequired("reason", data["reason"]); err != nil {
        errs = append(errs, err)
    }

    return errs
}
