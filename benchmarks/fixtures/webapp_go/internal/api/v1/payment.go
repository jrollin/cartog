package v1

import (
    "webapp_go/internal/routes"
    "webapp_go/internal/validators"
    "webapp_go/pkg/logger"
)

var payV1Log = logger.GetLogger("api.v1.payment")

// HandlePayment handles v1 payment endpoint.
func HandlePayment(request map[string]interface{}) (map[string]interface{}, error) {
    payV1Log.Info("V1 HandlePayment")

    validator := validators.NewPaymentValidator()
    amount, _ := request["amount"].(string)
    currency, _ := request["currency"].(string)
    userID, _ := request["user_id"].(string)

    errs := validator.Validate(map[string]string{
        "amount": amount, "currency": currency, "user_id": userID,
    })
    if len(errs) > 0 {
        payV1Log.Error("Payment validation failed")
        return nil, errs[0]
    }

    result, err := routes.PaymentHandler(request)
    if err != nil {
        return nil, err
    }
    result["api_version"] = "v1"
    payV1Log.Info("V1 payment complete")
    return result, nil
}
