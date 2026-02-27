package models

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var userLog = logger.GetLogger("models.user")

// User represents a user in the application.
type User struct {
    ID        string
    Email     string
    Name      string
    Password  string
    Role      UserRole
    Active    bool
    CreatedAt string
    UpdatedAt string
    Metadata  map[string]interface{}
}

// NewUser creates a new user with default values.
func NewUser(email, name, password string) *User {
    userLog.Info("Creating new user: %s", email)
    return &User{
        ID:       fmt.Sprintf("usr_%s", email),
        Email:    email,
        Name:     name,
        Password: password,
        Role:     RoleUser,
        Active:   true,
        Metadata: make(map[string]interface{}),
    }
}

// Validate checks that the user has valid field values.
func (u *User) Validate() []string {
    userLog.Debug("Validating user: %s", u.Email)
    var errs []string
    if u.Email == "" {
        errs = append(errs, "email is required")
    }
    if u.Name == "" {
        errs = append(errs, "name is required")
    }
    if len(u.Password) < 8 {
        errs = append(errs, "password must be at least 8 characters")
    }
    if len(errs) > 0 {
        userLog.Warn("User validation failed with %d errors", len(errs))
    }
    return errs
}

// FullName returns the user's display name.
func (u *User) FullName() string {
    return u.Name
}

// IsAdmin checks if the user has admin role.
func (u *User) IsAdmin() bool {
    return u.Role == RoleAdmin || u.Role == RoleSuperAdmin
}

// Deactivate disables the user account.
func (u *User) Deactivate() {
    userLog.Info("Deactivating user: %s", u.Email)
    u.Active = false
}

// SetMetadata sets a metadata key-value pair.
func (u *User) SetMetadata(key string, value interface{}) {
    userLog.Debug("Setting metadata for user %s: %s", u.Email, key)
    u.Metadata[key] = value
}
