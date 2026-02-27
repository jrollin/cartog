package auth

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var serviceLog = logger.GetLogger("auth.service")

// User represents an authenticated user.
type User struct {
    ID       string
    Email    string
    Password string
    Name     string
    Role     string
    Active   bool
}

// AuthProvider defines the interface for authentication providers.
type AuthProvider interface {
    Login(email, password string) (string, error)
    Logout(token string) error
}

// BaseService provides common service functionality.
type BaseService struct {
    Name    string
    Version string
}

// Initialize sets up the base service.
func (b *BaseService) Initialize() {
    serviceLog.Info("Initializing service: %s v%s", b.Name, b.Version)
}

// AuthService handles user authentication.
type AuthService struct {
    BaseService
    Users map[string]*User
}

// NewAuthService creates a new authentication service.
func NewAuthService() *AuthService {
    svc := &AuthService{
        BaseService: BaseService{Name: "auth", Version: "1.0"},
        Users:       make(map[string]*User),
    }
    svc.Initialize()
    serviceLog.Info("AuthService created")
    return svc
}

// Login authenticates a user and returns a token.
func (s *AuthService) Login(email, password string) (string, error) {
    serviceLog.Info("Login attempt for: %s", email)
    user, ok := s.Users[email]
    if !ok {
        serviceLog.Warn("User not found: %s", email)
        return "", fmt.Errorf("user not found: %s", email)
    }
    if user.Password != password {
        serviceLog.Warn("Invalid password for: %s", email)
        return "", fmt.Errorf("invalid credentials")
    }
    if !user.Active {
        serviceLog.Warn("Inactive user: %s", email)
        return "", fmt.Errorf("account disabled")
    }
    token := GenerateToken(*user)
    serviceLog.Info("Login successful for: %s", email)
    return token, nil
}

// Logout invalidates a user's token.
func (s *AuthService) Logout(token string) error {
    serviceLog.Info("Logout request")
    return RevokeToken(token)
}

// GetCurrentUser returns the user associated with a token.
func (s *AuthService) GetCurrentUser(token string) (*User, error) {
    serviceLog.Info("Getting current user from token")
    claims, err := ValidateToken(token)
    if err != nil {
        serviceLog.Error("Invalid token: %v", err)
        return nil, err
    }
    user := &User{
        ID:    claims.UserID,
        Email: claims.Email,
        Role:  claims.Role,
    }
    serviceLog.Info("Current user: %s", user.Email)
    return user, nil
}

// AdminService extends AuthService with admin-specific functionality.
type AdminService struct {
    AuthService
    AdminUsers []string
}

// NewAdminService creates a new admin service.
func NewAdminService() *AdminService {
    svc := &AdminService{
        AuthService: *NewAuthService(),
        AdminUsers:  []string{},
    }
    serviceLog.Info("AdminService created")
    return svc
}

// IsAdmin checks whether a user has admin privileges.
func (a *AdminService) IsAdmin(userID string) bool {
    serviceLog.Debug("Checking admin status for: %s", userID)
    for _, id := range a.AdminUsers {
        if id == userID {
            return true
        }
    }
    return false
}

// PromoteToAdmin grants admin privileges to a user.
func (a *AdminService) PromoteToAdmin(userID string) {
    serviceLog.Info("Promoting user to admin: %s", userID)
    a.AdminUsers = append(a.AdminUsers, userID)
}
