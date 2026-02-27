package models

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var paymentLog = logger.GetLogger("models.payment")

// Payment represents a financial transaction.
type Payment struct {
    ID            string
    UserID        string
    Amount        float64
    Currency      string
    Status        PaymentStatus
    TransactionID string
    Description   string
    CreatedAt     string
    UpdatedAt     string
    Metadata      map[string]interface{}
}

// NewPayment creates a new payment record.
func NewPayment(userID string, amount float64, currency, description string) *Payment {
    paymentLog.Info("Creating payment: user=%s, amount=%.2f %s", userID, amount, currency)
    return &Payment{
        ID:          fmt.Sprintf("pay_%s", userID),
        UserID:      userID,
        Amount:      amount,
        Currency:    currency,
        Status:      PaymentPending,
        Description: description,
        Metadata:    make(map[string]interface{}),
    }
}

// Validate checks that the payment has valid field values.
func (p *Payment) Validate() []string {
    paymentLog.Debug("Validating payment: %s", p.ID)
    var errs []string
    if p.Amount <= 0 {
        errs = append(errs, "amount must be positive")
    }
    if p.Currency == "" {
        errs = append(errs, "currency is required")
    }
    if p.UserID == "" {
        errs = append(errs, "user ID is required")
    }
    if len(errs) > 0 {
        paymentLog.Warn("Payment validation failed with %d errors", len(errs))
    }
    return errs
}

// Process moves the payment to processing state.
func (p *Payment) Process() error {
    paymentLog.Info("Processing payment: %s", p.ID)
    if p.Status != PaymentPending {
        return fmt.Errorf("cannot process payment in %s state", p.Status.String())
    }
    p.Status = PaymentProcessing
    return nil
}

// Complete marks the payment as completed.
func (p *Payment) Complete(txnID string) {
    paymentLog.Info("Completing payment: %s, txn=%s", p.ID, txnID)
    p.Status = PaymentCompleted
    p.TransactionID = txnID
}

// Fail marks the payment as failed.
func (p *Payment) Fail(reason string) {
    paymentLog.Error("Payment failed: %s, reason=%s", p.ID, reason)
    p.Status = PaymentFailed
    p.Metadata["failure_reason"] = reason
}

// Refund marks the payment as refunded.
func (p *Payment) Refund() error {
    paymentLog.Info("Refunding payment: %s", p.ID)
    if p.Status != PaymentCompleted {
        return fmt.Errorf("cannot refund payment in %s state", p.Status.String())
    }
    p.Status = PaymentRefunded
    return nil
}
