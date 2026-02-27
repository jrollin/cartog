package payment

import (
    "fmt"

    "webapp_go/internal/database"
    "webapp_go/internal/models"
    "webapp_go/pkg/logger"
)

var procLog = logger.GetLogger("services.payment.processor")

// PaymentProcessor handles payment processing workflows.
type PaymentProcessor struct {
    DB      *database.DatabaseConnection
    Gateway *PaymentGateway
}

// NewPaymentProcessor creates a new processor with a gateway.
func NewPaymentProcessor(db *database.DatabaseConnection) *PaymentProcessor {
    procLog.Info("Creating PaymentProcessor")
    return &PaymentProcessor{
        DB:      db,
        Gateway: NewPaymentGateway("stripe"),
    }
}

// Process handles the full payment lifecycle.
func (p *PaymentProcessor) Process(payment *models.Payment) error {
    procLog.Info("Processing payment: %s (amount=%.2f %s)", payment.ID, payment.Amount, payment.Currency)

    errs := payment.Validate()
    if len(errs) > 0 {
        procLog.Error("Payment validation failed: %v", errs)
        return fmt.Errorf("validation failed: %v", errs)
    }

    if err := payment.Process(); err != nil {
        procLog.Error("Cannot start processing: %v", err)
        return err
    }

    txnID, err := p.Gateway.Charge(payment.Amount, payment.Currency)
    if err != nil {
        procLog.Error("Gateway charge failed: %v", err)
        payment.Fail(err.Error())
        return err
    }

    payment.Complete(txnID)
    _, err = p.DB.Insert("payments", map[string]interface{}{
        "id":     payment.ID,
        "amount": payment.Amount,
        "txn_id": txnID,
    })
    if err != nil {
        procLog.Error("Failed to record payment: %v", err)
        return err
    }

    procLog.Info("Payment processed successfully: %s -> %s", payment.ID, txnID)
    return nil
}

// Refund processes a refund for a completed payment.
func (p *PaymentProcessor) Refund(paymentID string) error {
    procLog.Info("Refunding payment: %s", paymentID)
    _, err := p.DB.FindByID("payments", paymentID)
    if err != nil {
        procLog.Error("Payment not found: %v", err)
        return err
    }
    err = p.Gateway.Refund(paymentID)
    if err != nil {
        procLog.Error("Gateway refund failed: %v", err)
        return err
    }
    procLog.Info("Refund processed: %s", paymentID)
    return nil
}

// GetHistory retrieves payment history for a user.
func (p *PaymentProcessor) GetHistory(userID string) ([]map[string]interface{}, error) {
    procLog.Info("Getting payment history for user: %s", userID)
    return p.DB.ExecuteQuery("SELECT * FROM payments WHERE user_id = $1 ORDER BY created_at DESC", userID)
}
