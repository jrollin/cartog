package middleware

import (
    "webapp_go/pkg/logger"
)

var corsLog = logger.GetLogger("middleware.cors")

// CorsConfig defines CORS configuration.
type CorsConfig struct {
    AllowOrigins []string
    AllowMethods []string
    AllowHeaders []string
    MaxAge       int
}

// DefaultCorsConfig returns a permissive CORS configuration.
func DefaultCorsConfig() *CorsConfig {
    corsLog.Info("Loading default CORS config")
    return &CorsConfig{
        AllowOrigins: []string{"*"},
        AllowMethods: []string{"GET", "POST", "PUT", "DELETE", "OPTIONS"},
        AllowHeaders: []string{"Content-Type", "Authorization", "X-Request-ID"},
        MaxAge:       86400,
    }
}

// CorsMiddleware adds CORS headers to responses.
func CorsMiddleware(config *CorsConfig, next Handler) Handler {
    corsLog.Info("Installing CORS middleware")
    return func(req *Request) *Response {
        corsLog.Debug("Processing CORS for request")
        origin := req.Headers["Origin"]
        allowed := false
        for _, o := range config.AllowOrigins {
            if o == "*" || o == origin {
                allowed = true
                break
            }
        }
        if !allowed {
            corsLog.Warn("CORS blocked origin: %s", origin)
            return &Response{
                Status: 403,
                Body:   map[string]interface{}{"error": "origin not allowed"},
            }
        }
        corsLog.Debug("CORS allowed for origin: %s", origin)
        resp := next(req)
        if resp.Body == nil {
            resp.Body = make(map[string]interface{})
        }
        resp.Body["Access-Control-Allow-Origin"] = origin
        return resp
    }
}
