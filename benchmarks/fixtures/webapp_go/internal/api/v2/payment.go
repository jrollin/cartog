package v2

import (
    "webapp_go/internal/routes"
    "webapp_go/internal/validators"
    "webapp_go/pkg/logger"
)

var payV2Log = logger.GetLogger("api.v2.payment")

// HandlePayment handles v2 payment endpoint with enhanced validation.
func HandlePayment(request map[string]interface{}) (map[string]interface{}, error) {
    payV2Log.Info("V2 HandlePayment")

    validator := validators.NewPaymentValidator()
    amount, _ := request["amount"].(string)
    currency, _ := request["currency"].(string)
    userID, _ := request["user_id"].(string)

    errs := validator.Validate(map[string]string{
        "amount": amount, "currency": currency, "user_id": userID,
    })
    if len(errs) > 0 {
        payV2Log.Error("V2 payment validation failed")
        return nil, errs[0]
    }

    result, err := routes.PaymentHandler(request)
    if err != nil {
        return nil, err
    }
    result["api_version"] = "v2"
    result["idempotency_key"] = request["idempotency_key"]
    payV2Log.Info("V2 payment complete")
    return result, nil
}
