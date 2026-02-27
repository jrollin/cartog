package routes

import (
    "webapp_go/internal/auth"
    "webapp_go/internal/services"
    "webapp_go/internal/database"
    "webapp_go/pkg/logger"
)

var authRouteLog = logger.GetLogger("routes.auth")

// LoginHandler handles login requests.
func LoginHandler(request map[string]interface{}) (map[string]interface{}, error) {
    authRouteLog.Info("Login request received")

    email, _ := request["email"].(string)
    password, _ := request["password"].(string)

    db := database.NewDatabaseConnection("localhost", 5432, "app", "user")
    authSvc := services.NewAuthenticationService(db)

    token, err := authSvc.Authenticate(email, password)
    if err != nil {
        authRouteLog.Error("Login failed: %v", err)
        return map[string]interface{}{"error": err.Error()}, err
    }

    authRouteLog.Info("Login successful for: %s", email)
    return map[string]interface{}{
        "token": token,
        "user":  email,
    }, nil
}

// LogoutHandler handles logout requests.
func LogoutHandler(request map[string]interface{}) (map[string]interface{}, error) {
    authRouteLog.Info("Logout request received")
    headers, _ := request["headers"].(map[string]string)
    token := auth.ExtractToken(headers)
    err := auth.RevokeToken(token)
    if err != nil {
        authRouteLog.Error("Logout failed: %v", err)
        return nil, err
    }
    authRouteLog.Info("Logout successful")
    return map[string]interface{}{"status": "logged_out"}, nil
}

// RefreshHandler handles token refresh requests.
func RefreshHandler(request map[string]interface{}) (map[string]interface{}, error) {
    authRouteLog.Info("Token refresh request")
    headers, _ := request["headers"].(map[string]string)
    oldToken := auth.ExtractToken(headers)
    newToken, err := auth.RefreshToken(oldToken)
    if err != nil {
        authRouteLog.Error("Token refresh failed: %v", err)
        return nil, err
    }
    return map[string]interface{}{"token": newToken}, nil
}
