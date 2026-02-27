package payment

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var gwLog = logger.GetLogger("services.payment.gateway")

// PaymentGateway communicates with an external payment provider.
type PaymentGateway struct {
    Provider  string
    APIKey    string
    BaseURL   string
    Connected bool
}

// NewPaymentGateway creates a gateway for the specified provider.
func NewPaymentGateway(provider string) *PaymentGateway {
    gwLog.Info("Creating PaymentGateway for provider: %s", provider)
    return &PaymentGateway{
        Provider:  provider,
        APIKey:    "sk_test_placeholder",
        BaseURL:   fmt.Sprintf("https://api.%s.com/v1", provider),
        Connected: true,
    }
}

// Charge sends a charge request to the payment provider.
func (g *PaymentGateway) Charge(amount float64, currency string) (string, error) {
    gwLog.Info("Charging %.2f %s via %s", amount, currency, g.Provider)
    if !g.Connected {
        gwLog.Error("Gateway not connected")
        return "", fmt.Errorf("gateway not connected")
    }
    if amount <= 0 {
        gwLog.Error("Invalid charge amount: %.2f", amount)
        return "", fmt.Errorf("invalid amount")
    }
    txnID := fmt.Sprintf("txn_%s_%.0f", g.Provider, amount*100)
    gwLog.Info("Charge successful: %s", txnID)
    return txnID, nil
}

// Refund sends a refund request to the payment provider.
func (g *PaymentGateway) Refund(transactionID string) error {
    gwLog.Info("Refunding transaction: %s via %s", transactionID, g.Provider)
    if !g.Connected {
        gwLog.Error("Gateway not connected")
        return fmt.Errorf("gateway not connected")
    }
    gwLog.Info("Refund successful for: %s", transactionID)
    return nil
}

// GetBalance retrieves the current account balance.
func (g *PaymentGateway) GetBalance() (float64, error) {
    gwLog.Info("Getting balance from %s", g.Provider)
    if !g.Connected {
        return 0, fmt.Errorf("gateway not connected")
    }
    return 10000.00, nil
}

// Disconnect closes the gateway connection.
func (g *PaymentGateway) Disconnect() {
    gwLog.Info("Disconnecting from %s", g.Provider)
    g.Connected = false
}
