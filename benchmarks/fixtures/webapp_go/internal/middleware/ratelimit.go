package middleware

import (
    "sync"

    "webapp_go/pkg/logger"
)

var rlLog = logger.GetLogger("middleware.ratelimit")

// RateLimiter tracks request rates per client.
type RateLimiter struct {
    Requests map[string]int
    Limit    int
    mu       sync.Mutex
}

// NewRateLimiter creates a rate limiter with the specified limit.
func NewRateLimiter(limit int) *RateLimiter {
    rlLog.Info("Creating RateLimiter with limit: %d", limit)
    return &RateLimiter{
        Requests: make(map[string]int),
        Limit:    limit,
    }
}

// RateLimitMiddleware limits request rates per client IP.
func RateLimitMiddleware(limiter *RateLimiter, next Handler) Handler {
    rlLog.Info("Installing rate limit middleware")
    return func(req *Request) *Response {
        ip := req.Headers["X-Forwarded-For"]
        if ip == "" {
            ip = "unknown"
        }
        rlLog.Debug("Rate check for IP: %s", ip)

        limiter.mu.Lock()
        limiter.Requests[ip]++
        count := limiter.Requests[ip]
        limiter.mu.Unlock()

        if count > limiter.Limit {
            rlLog.Warn("Rate limit exceeded for IP: %s (%d requests)", ip, count)
            return &Response{
                Status: 429,
                Body:   map[string]interface{}{"error": "rate limit exceeded"},
            }
        }
        rlLog.Debug("Rate check passed for IP: %s (%d/%d)", ip, count, limiter.Limit)
        return next(req)
    }
}

// Reset clears all rate limit counters.
func (r *RateLimiter) Reset() {
    r.mu.Lock()
    defer r.mu.Unlock()
    rlLog.Info("Resetting rate limiter")
    r.Requests = make(map[string]int)
}
