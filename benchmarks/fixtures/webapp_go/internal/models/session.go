package models

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var sessionLog = logger.GetLogger("models.session")

// Session represents an active user session.
type Session struct {
    ID        string
    UserID    string
    Token     string
    Status    SessionStatus
    IPAddress string
    UserAgent string
    CreatedAt string
    ExpiresAt string
}

// NewSession creates a new session for a user.
func NewSession(userID, token, ip, userAgent string) *Session {
    sessionLog.Info("Creating new session for user: %s", userID)
    return &Session{
        ID:        fmt.Sprintf("sess_%s", userID),
        UserID:    userID,
        Token:     token,
        Status:    SessionActive,
        IPAddress: ip,
        UserAgent: userAgent,
    }
}

// IsValid checks if the session is still active.
func (s *Session) IsValid() bool {
    sessionLog.Debug("Checking session validity: %s", s.ID)
    return s.Status == SessionActive
}

// Expire marks the session as expired.
func (s *Session) Expire() {
    sessionLog.Info("Expiring session: %s", s.ID)
    s.Status = SessionExpired
}

// Revoke marks the session as revoked.
func (s *Session) Revoke() {
    sessionLog.Info("Revoking session: %s", s.ID)
    s.Status = SessionRevoked
}

// Suspend marks the session as suspended.
func (s *Session) Suspend() {
    sessionLog.Info("Suspending session: %s", s.ID)
    s.Status = SessionSuspended
}

// Refresh extends the session expiry.
func (s *Session) Refresh() {
    sessionLog.Info("Refreshing session: %s", s.ID)
    s.Status = SessionActive
}
