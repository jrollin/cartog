package services

import (
    "webapp_go/pkg/logger"
)

var baseLog = logger.GetLogger("services.base")

// Service defines the interface that all services must implement.
type Service interface {
    Name() string
    Initialize() error
    Shutdown() error
}

// CacheableService extends Service with caching capabilities.
type CacheableService interface {
    Service
    CacheKey(operation string, params ...interface{}) string
    InvalidateCache(pattern string) error
}

// AuditableService extends Service with audit logging.
type AuditableService interface {
    Service
    AuditLog(action, userID string, details map[string]interface{}) error
}

// BaseServiceImpl provides common functionality for all services.
type BaseServiceImpl struct {
    ServiceName    string
    ServiceVersion string
}

// Name returns the service name.
func (b *BaseServiceImpl) Name() string {
    return b.ServiceName
}

// Initialize sets up the service.
func (b *BaseServiceImpl) Initialize() error {
    baseLog.Info("Initializing service: %s v%s", b.ServiceName, b.ServiceVersion)
    return nil
}

// Shutdown tears down the service.
func (b *BaseServiceImpl) Shutdown() error {
    baseLog.Info("Shutting down service: %s", b.ServiceName)
    return nil
}
