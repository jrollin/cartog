package cache

import (
    "webapp_go/pkg/logger"
)

var cacheLog = logger.GetLogger("cache")

// Cache defines the interface for cache implementations.
type Cache interface {
    Get(key string) (interface{}, bool)
    Set(key string, value interface{}, ttl int) error
    Delete(key string) error
    Clear() error
    Has(key string) bool
}

// CacheEntry represents a single cached item.
type CacheEntry struct {
    Key       string
    Value     interface{}
    TTL       int
    CreatedAt int64
}

// LogCacheOperation logs a cache operation.
func LogCacheOperation(op, key string, hit bool) {
    if hit {
        cacheLog.Debug("Cache %s HIT: %s", op, key)
    } else {
        cacheLog.Debug("Cache %s MISS: %s", op, key)
    }
}
