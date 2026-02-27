package middleware

import (
    "time"

    "webapp_go/pkg/logger"
)

var reqLog = logger.GetLogger("middleware.logging")

// LoggingMiddleware logs all incoming requests and their response times.
func LoggingMiddleware(next Handler) Handler {
    reqLog.Info("Installing logging middleware")
    return func(req *Request) *Response {
        start := time.Now()
        method := req.Headers["Method"]
        path := req.Headers["Path"]
        requestID := req.Headers["X-Request-ID"]

        reqLog.Info("Request started: %s %s (id=%s)", method, path, requestID)

        resp := next(req)

        duration := time.Since(start)
        reqLog.Info("Request completed: %s %s -> %d (%.2fms)",
            method, path, resp.Status, float64(duration.Microseconds())/1000.0)

        if resp.Status >= 400 {
            reqLog.Warn("Error response: %s %s -> %d", method, path, resp.Status)
        }

        return resp
    }
}

// RequestTimingMiddleware adds timing headers to responses.
func RequestTimingMiddleware(next Handler) Handler {
    reqLog.Info("Installing timing middleware")
    return func(req *Request) *Response {
        start := time.Now()
        resp := next(req)
        duration := time.Since(start)
        if resp.Body == nil {
            resp.Body = make(map[string]interface{})
        }
        resp.Body["X-Response-Time"] = duration.String()
        return resp
    }
}
