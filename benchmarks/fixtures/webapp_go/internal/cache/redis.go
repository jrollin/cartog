package cache

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var redisLog = logger.GetLogger("cache.redis")

// RedisCache implements Cache using Redis as the backend.
type RedisCache struct {
    Host     string
    Port     int
    Password string
    DB       int
    store    map[string]*CacheEntry
}

// NewRedisCache creates a new Redis-backed cache.
func NewRedisCache(host string, port int, password string, db int) *RedisCache {
    redisLog.Info("Creating RedisCache: %s:%d (db=%d)", host, port, db)
    return &RedisCache{
        Host:     host,
        Port:     port,
        Password: password,
        DB:       db,
        store:    make(map[string]*CacheEntry),
    }
}

// Get retrieves a value from the cache.
func (r *RedisCache) Get(key string) (interface{}, bool) {
    redisLog.Debug("GET %s", key)
    entry, ok := r.store[key]
    LogCacheOperation("GET", key, ok)
    if !ok {
        return nil, false
    }
    return entry.Value, true
}

// Set stores a value in the cache with a TTL.
func (r *RedisCache) Set(key string, value interface{}, ttl int) error {
    redisLog.Debug("SET %s (ttl=%d)", key, ttl)
    r.store[key] = &CacheEntry{Key: key, Value: value, TTL: ttl}
    return nil
}

// Delete removes a key from the cache.
func (r *RedisCache) Delete(key string) error {
    redisLog.Debug("DEL %s", key)
    delete(r.store, key)
    return nil
}

// Clear removes all keys from the cache.
func (r *RedisCache) Clear() error {
    redisLog.Info("CLEAR all keys")
    r.store = make(map[string]*CacheEntry)
    return nil
}

// Has checks if a key exists in the cache.
func (r *RedisCache) Has(key string) bool {
    _, ok := r.store[key]
    return ok
}

// ConnectionString returns the Redis connection URI.
func (r *RedisCache) ConnectionString() string {
    return fmt.Sprintf("redis://:%s@%s:%d/%d", r.Password, r.Host, r.Port, r.DB)
}
