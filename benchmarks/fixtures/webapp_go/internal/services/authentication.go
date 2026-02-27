package services

import (
    "fmt"

    "webapp_go/internal/auth"
    "webapp_go/internal/database"
    "webapp_go/pkg/logger"
)

var authLog = logger.GetLogger("services.authentication")

// AuthenticationService handles user authentication workflows.
type AuthenticationService struct {
    BaseServiceImpl
    AuthSvc *auth.AuthService
    DB      *database.DatabaseConnection
}

// NewAuthenticationService creates a new authentication service.
func NewAuthenticationService(db *database.DatabaseConnection) *AuthenticationService {
    authLog.Info("Creating AuthenticationService")
    svc := &AuthenticationService{
        BaseServiceImpl: BaseServiceImpl{
            ServiceName:    "authentication",
            ServiceVersion: "1.0",
        },
        AuthSvc: auth.NewAuthService(),
        DB:      db,
    }
    svc.Initialize()
    return svc
}

// Authenticate performs the full authentication flow:
// auth.Login -> GenerateToken -> ExecuteQuery -> GetConnection
func (s *AuthenticationService) Authenticate(email, password string) (string, error) {
    authLog.Info("Authenticating user: %s", email)

    // Step 1: Login via auth service
    token, err := s.AuthSvc.Login(email, password)
    if err != nil {
        authLog.Error("Login failed for %s: %v", email, err)
        return "", fmt.Errorf("authentication failed: %w", err)
    }

    // Step 2: Generate a fresh token
    user := auth.User{ID: "user_1", Email: email}
    freshToken := auth.GenerateToken(user)

    // Step 3: Record login in database
    handle, err := s.DB.Pool.GetConnection()
    if err != nil {
        authLog.Error("DB connection failed: %v", err)
        return "", fmt.Errorf("database error: %w", err)
    }
    defer s.DB.Pool.ReleaseConnection(handle)

    _, err = s.DB.ExecuteQuery("INSERT INTO sessions (token, user) VALUES ($1, $2)", freshToken, email)
    if err != nil {
        authLog.Error("Session insert failed: %v", err)
        return "", fmt.Errorf("session error: %w", err)
    }

    authLog.Info("Authentication successful for: %s (token=%s, orig=%s)", email, freshToken, token)
    return freshToken, nil
}

// Logout terminates a user session.
func (s *AuthenticationService) Logout(token string) error {
    authLog.Info("Logging out token")
    return s.AuthSvc.Logout(token)
}

// GetCurrentUser retrieves the authenticated user.
func (s *AuthenticationService) GetCurrentUser(token string) (*auth.User, error) {
    authLog.Info("Getting current user")
    return s.AuthSvc.GetCurrentUser(token)
}
