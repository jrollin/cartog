package middleware

import (
    "fmt"

    "webapp_go/internal/auth"
    "webapp_go/pkg/logger"
)

var authMwLog = logger.GetLogger("middleware.auth")

// Request represents an HTTP request.
type Request struct {
    Headers map[string]string
    Body    map[string]interface{}
    User    *auth.TokenClaims
    Params  map[string]string
}

// Response represents an HTTP response.
type Response struct {
    Status int
    Body   map[string]interface{}
}

// Handler is a middleware-compatible handler function.
type Handler func(*Request) *Response

// AuthMiddleware verifies authentication on incoming requests.
func AuthMiddleware(next Handler) Handler {
    authMwLog.Info("Installing auth middleware")
    return func(req *Request) *Response {
        authMwLog.Debug("Checking authentication")
        token := auth.ExtractToken(req.Headers)
        if token == "" {
            authMwLog.Warn("No token found")
            return &Response{Status: 401, Body: map[string]interface{}{"error": "unauthorized"}}
        }
        claims, err := auth.ValidateToken(token)
        if err != nil {
            authMwLog.Error("Token validation failed: %v", err)
            return &Response{Status: 401, Body: map[string]interface{}{"error": fmt.Sprintf("invalid token: %v", err)}}
        }
        req.User = claims
        authMwLog.Info("Authenticated: %s", claims.UserID)
        return next(req)
    }
}

// AdminMiddleware requires admin role.
func AdminMiddleware(next Handler) Handler {
    authMwLog.Info("Installing admin middleware")
    return AuthMiddleware(func(req *Request) *Response {
        if req.User.Role != "admin" {
            authMwLog.Warn("Non-admin access attempt by: %s", req.User.UserID)
            return &Response{Status: 403, Body: map[string]interface{}{"error": "forbidden"}}
        }
        return next(req)
    })
}
