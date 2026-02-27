package tasks

import (
    "webapp_go/internal/database"
    "webapp_go/internal/models"
    "webapp_go/internal/services/payment"
    "webapp_go/pkg/logger"
)

var payTaskLog = logger.GetLogger("tasks.payment")

// PaymentTask represents a background payment processing task.
type PaymentTask struct {
    Processor *payment.PaymentProcessor
    Payments  []*models.Payment
    Processed int
    Failed    int
}

// NewPaymentTask creates a new payment processing task.
func NewPaymentTask(db *database.DatabaseConnection) *PaymentTask {
    payTaskLog.Info("Creating PaymentTask")
    return &PaymentTask{
        Processor: payment.NewPaymentProcessor(db),
        Payments:  make([]*models.Payment, 0),
        Processed: 0,
        Failed:    0,
    }
}

// AddPayment queues a payment for processing.
func (t *PaymentTask) AddPayment(p *models.Payment) {
    payTaskLog.Info("Queuing payment: %s", p.ID)
    t.Payments = append(t.Payments, p)
}

// Execute processes all queued payments.
func (t *PaymentTask) Execute() error {
    payTaskLog.Info("Executing payment task: %d payments", len(t.Payments))
    for _, p := range t.Payments {
        err := t.Processor.Process(p)
        if err != nil {
            payTaskLog.Error("Payment failed: %s - %v", p.ID, err)
            t.Failed++
            continue
        }
        t.Processed++
    }
    payTaskLog.Info("Payment task complete: %d processed, %d failed", t.Processed, t.Failed)
    return nil
}

// Status returns the task summary.
func (t *PaymentTask) Status() map[string]int {
    return map[string]int{
        "total":     len(t.Payments),
        "processed": t.Processed,
        "failed":    t.Failed,
    }
}
