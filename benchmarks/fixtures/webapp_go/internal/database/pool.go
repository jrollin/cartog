package database

import (
    "fmt"
    "sync"

    "webapp_go/pkg/logger"
)

var poolLog = logger.GetLogger("database.pool")

// ConnectionHandle wraps a database connection with metadata.
type ConnectionHandle struct {
    ID       int
    InUse    bool
    Database string
}

// ConnectionPool manages a pool of database connections.
type ConnectionPool struct {
    connections []*ConnectionHandle
    maxSize     int
    mu          sync.Mutex
}

// NewConnectionPool creates a pool with the specified max size.
func NewConnectionPool(maxSize int) *ConnectionPool {
    poolLog.Info("Creating connection pool with max size: %d", maxSize)
    pool := &ConnectionPool{
        connections: make([]*ConnectionHandle, 0, maxSize),
        maxSize:     maxSize,
    }
    for i := 0; i < maxSize; i++ {
        pool.connections = append(pool.connections, &ConnectionHandle{
            ID:       i,
            InUse:    false,
            Database: "default",
        })
    }
    poolLog.Info("Connection pool initialized with %d connections", maxSize)
    return pool
}

// GetConnection acquires a connection from the pool.
func (p *ConnectionPool) GetConnection() (*ConnectionHandle, error) {
    p.mu.Lock()
    defer p.mu.Unlock()
    poolLog.Debug("Requesting connection from pool")
    for _, conn := range p.connections {
        if !conn.InUse {
            conn.InUse = true
            poolLog.Info("Acquired connection #%d", conn.ID)
            return conn, nil
        }
    }
    poolLog.Error("No available connections in pool")
    return nil, fmt.Errorf("connection pool exhausted")
}

// ReleaseConnection returns a connection to the pool.
func (p *ConnectionPool) ReleaseConnection(handle *ConnectionHandle) {
    p.mu.Lock()
    defer p.mu.Unlock()
    poolLog.Debug("Releasing connection #%d", handle.ID)
    handle.InUse = false
}

// ActiveCount returns the number of connections currently in use.
func (p *ConnectionPool) ActiveCount() int {
    p.mu.Lock()
    defer p.mu.Unlock()
    count := 0
    for _, conn := range p.connections {
        if conn.InUse {
            count++
        }
    }
    poolLog.Debug("Active connections: %d", count)
    return count
}

// Shutdown closes all connections in the pool.
func (p *ConnectionPool) Shutdown() {
    p.mu.Lock()
    defer p.mu.Unlock()
    poolLog.Info("Shutting down connection pool")
    for _, conn := range p.connections {
        conn.InUse = false
    }
    p.connections = p.connections[:0]
    poolLog.Info("Connection pool shut down")
}
