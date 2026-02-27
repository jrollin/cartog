package events

import (
    "webapp_go/pkg/logger"
)

var handlerLog = logger.GetLogger("events.handlers")

// UserCreatedHandler handles user creation events.
func UserCreatedHandler(event *Event) error {
    handlerLog.Info("Handling user.created event")
    email, _ := event.Payload["email"].(string)
    handlerLog.Info("New user created: %s", email)
    return nil
}

// UserDeletedHandler handles user deletion events.
func UserDeletedHandler(event *Event) error {
    handlerLog.Info("Handling user.deleted event")
    userID, _ := event.Payload["user_id"].(string)
    handlerLog.Info("User deleted: %s", userID)
    return nil
}

// PaymentCompletedHandler handles payment completion events.
func PaymentCompletedHandler(event *Event) error {
    handlerLog.Info("Handling payment.completed event")
    txnID, _ := event.Payload["txn_id"].(string)
    amount, _ := event.Payload["amount"].(float64)
    handlerLog.Info("Payment completed: txn=%s, amount=%.2f", txnID, amount)
    return nil
}

// PaymentFailedHandler handles payment failure events.
func PaymentFailedHandler(event *Event) error {
    handlerLog.Info("Handling payment.failed event")
    reason, _ := event.Payload["reason"].(string)
    handlerLog.Warn("Payment failed: %s", reason)
    return nil
}

// SessionExpiredHandler handles session expiry events.
func SessionExpiredHandler(event *Event) error {
    handlerLog.Info("Handling session.expired event")
    sessionID, _ := event.Payload["session_id"].(string)
    handlerLog.Info("Session expired: %s", sessionID)
    return nil
}

// RegisterDefaultHandlers sets up the default event handlers.
func RegisterDefaultHandlers(dispatcher *EventDispatcher) {
    handlerLog.Info("Registering default event handlers")
    dispatcher.On("user.created", UserCreatedHandler)
    dispatcher.On("user.deleted", UserDeletedHandler)
    dispatcher.On("payment.completed", PaymentCompletedHandler)
    dispatcher.On("payment.failed", PaymentFailedHandler)
    dispatcher.On("session.expired", SessionExpiredHandler)
    handlerLog.Info("Default handlers registered")
}
