package cache

import (
    "sync"

    "webapp_go/pkg/logger"
)

var memLog = logger.GetLogger("cache.memory")

// MemoryCache implements Cache using in-memory storage.
type MemoryCache struct {
    store map[string]*CacheEntry
    mu    sync.RWMutex
}

// NewMemoryCache creates a new in-memory cache.
func NewMemoryCache() *MemoryCache {
    memLog.Info("Creating MemoryCache")
    return &MemoryCache{
        store: make(map[string]*CacheEntry),
    }
}

// Get retrieves a value from memory.
func (m *MemoryCache) Get(key string) (interface{}, bool) {
    m.mu.RLock()
    defer m.mu.RUnlock()
    memLog.Debug("GET %s", key)
    entry, ok := m.store[key]
    LogCacheOperation("GET", key, ok)
    if !ok {
        return nil, false
    }
    return entry.Value, true
}

// Set stores a value in memory.
func (m *MemoryCache) Set(key string, value interface{}, ttl int) error {
    m.mu.Lock()
    defer m.mu.Unlock()
    memLog.Debug("SET %s (ttl=%d)", key, ttl)
    m.store[key] = &CacheEntry{Key: key, Value: value, TTL: ttl}
    return nil
}

// Delete removes a key from memory.
func (m *MemoryCache) Delete(key string) error {
    m.mu.Lock()
    defer m.mu.Unlock()
    memLog.Debug("DEL %s", key)
    delete(m.store, key)
    return nil
}

// Clear removes all keys from memory.
func (m *MemoryCache) Clear() error {
    m.mu.Lock()
    defer m.mu.Unlock()
    memLog.Info("CLEAR all keys")
    m.store = make(map[string]*CacheEntry)
    return nil
}

// Has checks if a key exists in memory.
func (m *MemoryCache) Has(key string) bool {
    m.mu.RLock()
    defer m.mu.RUnlock()
    _, ok := m.store[key]
    return ok
}

// Size returns the number of entries in the cache.
func (m *MemoryCache) Size() int {
    m.mu.RLock()
    defer m.mu.RUnlock()
    return len(m.store)
}
