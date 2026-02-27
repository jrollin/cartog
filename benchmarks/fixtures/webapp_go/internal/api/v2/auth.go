package v2

import (
    "fmt"

    "webapp_go/internal/routes"
    "webapp_go/internal/validators"
    "webapp_go/pkg/logger"
)

var authV2Log = logger.GetLogger("api.v2.auth")

// Validate checks v2 auth request parameters (stricter than v1).
func Validate(request map[string]interface{}) error {
    authV2Log.Info("Validating v2 auth request")
    email, _ := request["email"].(string)
    password, _ := request["password"].(string)
    validator := &validators.CommonValidators{}
    if err := validator.ValidateRequired("email", email); err != nil {
        return fmt.Errorf("email required")
    }
    if err := validator.ValidateEmail(email); err != nil {
        return fmt.Errorf("invalid email format")
    }
    if err := validator.ValidateRequired("password", password); err != nil {
        return fmt.Errorf("password required")
    }
    if err := validator.ValidateMinLength("password", password, 12); err != nil {
        return fmt.Errorf("password must be at least 12 characters in v2")
    }
    authV2Log.Info("V2 auth request validated")
    return nil
}

// HandleLogin handles v2 login endpoint with enhanced security.
func HandleLogin(request map[string]interface{}) (map[string]interface{}, error) {
    authV2Log.Info("V2 HandleLogin")
    if err := Validate(request); err != nil {
        authV2Log.Error("V2 validation failed: %v", err)
        return nil, err
    }
    result, err := routes.LoginHandler(request)
    if err != nil {
        return nil, err
    }
    result["api_version"] = "v2"
    result["enhanced_security"] = true
    authV2Log.Info("V2 login complete")
    return result, nil
}
