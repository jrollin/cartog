package v1

import (
    "fmt"

    "webapp_go/internal/routes"
    "webapp_go/internal/validators"
    "webapp_go/pkg/logger"
)

var authV1Log = logger.GetLogger("api.v1.auth")

// Validate checks v1 auth request parameters.
func Validate(request map[string]interface{}) error {
    authV1Log.Info("Validating v1 auth request")
    email, _ := request["email"].(string)
    password, _ := request["password"].(string)
    validator := &validators.CommonValidators{}
    if err := validator.ValidateRequired("email", email); err != nil {
        return fmt.Errorf("email required")
    }
    if err := validator.ValidateRequired("password", password); err != nil {
        return fmt.Errorf("password required")
    }
    if err := validator.ValidateEmail(email); err != nil {
        return fmt.Errorf("invalid email format")
    }
    authV1Log.Info("V1 auth request validated")
    return nil
}

// HandleLogin handles v1 login endpoint.
func HandleLogin(request map[string]interface{}) (map[string]interface{}, error) {
    authV1Log.Info("V1 HandleLogin")
    if err := Validate(request); err != nil {
        authV1Log.Error("Validation failed: %v", err)
        return nil, err
    }
    result, err := routes.LoginHandler(request)
    if err != nil {
        return nil, err
    }
    result["api_version"] = "v1"
    authV1Log.Info("V1 login complete")
    return result, nil
}
