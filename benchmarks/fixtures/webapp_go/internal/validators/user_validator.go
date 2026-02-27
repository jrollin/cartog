package validators

import (
    "webapp_go/pkg/logger"
)

var userValLog = logger.GetLogger("validators.user")

// UserValidator validates user-related data.
type UserValidator struct {
    CommonValidators
}

// NewUserValidator creates a new user validator.
func NewUserValidator() *UserValidator {
    userValLog.Info("Creating UserValidator")
    return &UserValidator{}
}

// Validate checks all user fields.
func (v *UserValidator) Validate(data map[string]string) []*ValidationError {
    userValLog.Info("Validating user data")
    var errs []*ValidationError

    if err := v.ValidateRequired("email", data["email"]); err != nil {
        errs = append(errs, err)
    } else if err := v.ValidateEmail(data["email"]); err != nil {
        errs = append(errs, err)
    }

    if err := v.ValidateRequired("name", data["name"]); err != nil {
        errs = append(errs, err)
    } else if err := v.ValidateMinLength("name", data["name"], 2); err != nil {
        errs = append(errs, err)
    }

    if err := v.ValidateRequired("password", data["password"]); err != nil {
        errs = append(errs, err)
    } else if err := v.ValidateMinLength("password", data["password"], 8); err != nil {
        errs = append(errs, err)
    }

    if len(errs) > 0 {
        userValLog.Warn("User validation failed: %d errors", len(errs))
    } else {
        userValLog.Info("User validation passed")
    }
    return errs
}

// ValidateUpdate checks fields for a user update.
func (v *UserValidator) ValidateUpdate(data map[string]string) []*ValidationError {
    userValLog.Info("Validating user update")
    var errs []*ValidationError

    if name, ok := data["name"]; ok {
        if err := v.ValidateMinLength("name", name, 2); err != nil {
            errs = append(errs, err)
        }
    }

    if email, ok := data["email"]; ok {
        if err := v.ValidateEmail(email); err != nil {
            errs = append(errs, err)
        }
    }

    return errs
}
