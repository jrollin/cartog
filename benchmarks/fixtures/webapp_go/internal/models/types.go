package models

import (
    "webapp_go/pkg/logger"
)

var log = logger.GetLogger("models.types")

// UserRole represents the role of a user in the system.
type UserRole int

const (
    RoleGuest UserRole = iota
    RoleUser
    RoleModerator
    RoleAdmin
    RoleSuperAdmin
)

// String returns the string representation of a UserRole.
func (r UserRole) String() string {
    switch r {
    case RoleGuest:
        return "guest"
    case RoleUser:
        return "user"
    case RoleModerator:
        return "moderator"
    case RoleAdmin:
        return "admin"
    case RoleSuperAdmin:
        return "super_admin"
    default:
        log.Warn("Unknown role: %d", r)
        return "unknown"
    }
}

// SessionStatus represents the state of a session.
type SessionStatus int

const (
    SessionActive SessionStatus = iota
    SessionExpired
    SessionRevoked
    SessionSuspended
)

// String returns the string representation of a SessionStatus.
func (s SessionStatus) String() string {
    switch s {
    case SessionActive:
        return "active"
    case SessionExpired:
        return "expired"
    case SessionRevoked:
        return "revoked"
    case SessionSuspended:
        return "suspended"
    default:
        log.Warn("Unknown session status: %d", s)
        return "unknown"
    }
}

// PaymentStatus represents the state of a payment.
type PaymentStatus int

const (
    PaymentPending PaymentStatus = iota
    PaymentProcessing
    PaymentCompleted
    PaymentFailed
    PaymentRefunded
    PaymentCancelled
)

// String returns the string representation of a PaymentStatus.
func (p PaymentStatus) String() string {
    switch p {
    case PaymentPending:
        return "pending"
    case PaymentProcessing:
        return "processing"
    case PaymentCompleted:
        return "completed"
    case PaymentFailed:
        return "failed"
    case PaymentRefunded:
        return "refunded"
    case PaymentCancelled:
        return "cancelled"
    default:
        log.Warn("Unknown payment status: %d", p)
        return "unknown"
    }
}
