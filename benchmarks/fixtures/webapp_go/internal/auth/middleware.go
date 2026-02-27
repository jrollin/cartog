package auth

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var mwLog = logger.GetLogger("auth.middleware")

// HandlerFunc represents an HTTP handler function.
type HandlerFunc func(map[string]interface{}) (map[string]interface{}, error)

// AuthRequired wraps a handler to require valid authentication.
func AuthRequired(handler HandlerFunc) HandlerFunc {
    mwLog.Info("Wrapping handler with auth requirement")
    return func(request map[string]interface{}) (map[string]interface{}, error) {
        mwLog.Debug("Checking authentication")
        headers, ok := request["headers"].(map[string]string)
        if !ok {
            mwLog.Error("No headers in request")
            return nil, fmt.Errorf("missing headers")
        }
        token := ExtractToken(headers)
        if token == "" {
            mwLog.Warn("No token found in request")
            return nil, fmt.Errorf("authentication required")
        }
        claims, err := ValidateToken(token)
        if err != nil {
            mwLog.Error("Token validation failed: %v", err)
            return nil, fmt.Errorf("invalid token: %w", err)
        }
        request["user"] = claims
        mwLog.Info("Authenticated user: %s", claims.UserID)
        return handler(request)
    }
}

// RequireRole wraps a handler to require a specific role.
func RequireRole(role string, handler HandlerFunc) HandlerFunc {
    mwLog.Info("Wrapping handler with role requirement: %s", role)
    return AuthRequired(func(request map[string]interface{}) (map[string]interface{}, error) {
        claims, ok := request["user"].(*TokenClaims)
        if !ok {
            mwLog.Error("No user claims in request")
            return nil, fmt.Errorf("no user claims")
        }
        if claims.Role != role {
            mwLog.Warn("User %s lacks role %s (has %s)", claims.UserID, role, claims.Role)
            return nil, fmt.Errorf("insufficient permissions: requires %s", role)
        }
        mwLog.Info("Role check passed for user: %s", claims.UserID)
        return handler(request)
    })
}

// RequireAnyRole wraps a handler to require one of several roles.
func RequireAnyRole(roles []string, handler HandlerFunc) HandlerFunc {
    mwLog.Info("Wrapping handler with any-role requirement")
    return AuthRequired(func(request map[string]interface{}) (map[string]interface{}, error) {
        claims, ok := request["user"].(*TokenClaims)
        if !ok {
            return nil, fmt.Errorf("no user claims")
        }
        for _, r := range roles {
            if claims.Role == r {
                return handler(request)
            }
        }
        mwLog.Warn("User %s lacks required roles", claims.UserID)
        return nil, fmt.Errorf("insufficient permissions")
    })
}
