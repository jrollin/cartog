package database

import (
    "webapp_go/pkg/logger"
)

var queryLog = logger.GetLogger("database.queries")

// UserQueries provides database operations for users.
type UserQueries struct {
    DB *DatabaseConnection
}

// FindByEmail looks up a user by email address.
func (q *UserQueries) FindByEmail(email string) (map[string]interface{}, error) {
    queryLog.Info("Finding user by email: %s", email)
    results, err := q.DB.ExecuteQuery("SELECT * FROM users WHERE email = $1", email)
    if err != nil {
        return nil, err
    }
    if len(results) == 0 {
        queryLog.Warn("User not found by email: %s", email)
        return nil, nil
    }
    return results[0], nil
}

// FindActive returns all active users.
func (q *UserQueries) FindActive() ([]map[string]interface{}, error) {
    queryLog.Info("Finding active users")
    return q.DB.ExecuteQuery("SELECT * FROM users WHERE active = true")
}

// SessionQueries provides database operations for sessions.
type SessionQueries struct {
    DB *DatabaseConnection
}

// FindByUserID returns all sessions for a user.
func (q *SessionQueries) FindByUserID(userID string) ([]map[string]interface{}, error) {
    queryLog.Info("Finding sessions for user: %s", userID)
    return q.DB.ExecuteQuery("SELECT * FROM sessions WHERE user_id = $1", userID)
}

// InvalidateAll removes all sessions for a user.
func (q *SessionQueries) InvalidateAll(userID string) error {
    queryLog.Info("Invalidating all sessions for user: %s", userID)
    _, err := q.DB.ExecuteQuery("DELETE FROM sessions WHERE user_id = $1", userID)
    return err
}

// PaymentQueries provides database operations for payments.
type PaymentQueries struct {
    DB *DatabaseConnection
}

// FindByTransactionID looks up a payment by transaction ID.
func (q *PaymentQueries) FindByTransactionID(txnID string) (map[string]interface{}, error) {
    queryLog.Info("Finding payment by transaction ID: %s", txnID)
    results, err := q.DB.ExecuteQuery("SELECT * FROM payments WHERE txn_id = $1", txnID)
    if err != nil {
        return nil, err
    }
    if len(results) == 0 {
        queryLog.Warn("Payment not found: %s", txnID)
        return nil, nil
    }
    return results[0], nil
}

// FindByUserID returns all payments for a user.
func (q *PaymentQueries) FindByUserID(userID string) ([]map[string]interface{}, error) {
    queryLog.Info("Finding payments for user: %s", userID)
    return q.DB.ExecuteQuery("SELECT * FROM payments WHERE user_id = $1", userID)
}

// SumByUserID calculates the total payment amount for a user.
func (q *PaymentQueries) SumByUserID(userID string) (float64, error) {
    queryLog.Info("Summing payments for user: %s", userID)
    _, err := q.DB.ExecuteQuery("SELECT SUM(amount) FROM payments WHERE user_id = $1", userID)
    if err != nil {
        return 0, err
    }
    return 0.0, nil
}
