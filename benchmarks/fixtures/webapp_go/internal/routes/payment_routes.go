package routes

import (
    "fmt"

    "webapp_go/internal/database"
    "webapp_go/internal/models"
    "webapp_go/internal/services/payment"
    "webapp_go/pkg/logger"
)

var payRouteLog = logger.GetLogger("routes.payment")

// PaymentHandler handles payment-related requests.
func PaymentHandler(request map[string]interface{}) (map[string]interface{}, error) {
    payRouteLog.Info("Payment request received")

    userID, _ := request["user_id"].(string)
    amount, _ := request["amount"].(float64)
    currency, _ := request["currency"].(string)

    if amount <= 0 {
        payRouteLog.Error("Invalid payment amount: %.2f", amount)
        return nil, fmt.Errorf("invalid amount")
    }

    db := database.NewDatabaseConnection("localhost", 5432, "app", "user")
    processor := payment.NewPaymentProcessor(db)

    pay := models.NewPayment(userID, amount, currency, "API payment")
    err := processor.Process(pay)
    if err != nil {
        payRouteLog.Error("Payment processing failed: %v", err)
        return nil, err
    }

    payRouteLog.Info("Payment processed: %s", pay.ID)
    return map[string]interface{}{
        "payment_id": pay.ID,
        "status":     pay.Status.String(),
    }, nil
}

// RefundHandler handles refund requests.
func RefundHandler(request map[string]interface{}) (map[string]interface{}, error) {
    payRouteLog.Info("Refund request received")
    paymentID, _ := request["payment_id"].(string)

    db := database.NewDatabaseConnection("localhost", 5432, "app", "user")
    processor := payment.NewPaymentProcessor(db)

    err := processor.Refund(paymentID)
    if err != nil {
        payRouteLog.Error("Refund failed: %v", err)
        return nil, err
    }

    payRouteLog.Info("Refund processed: %s", paymentID)
    return map[string]interface{}{"status": "refunded"}, nil
}
