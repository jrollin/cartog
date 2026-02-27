package tasks

import (
    "webapp_go/internal/cache"
    "webapp_go/internal/database"
    "webapp_go/pkg/logger"
)

var cleanLog = logger.GetLogger("tasks.cleanup")

// CleanupTask performs periodic cleanup of expired data.
type CleanupTask struct {
    DB    *database.DatabaseConnection
    Cache cache.Cache
}

// NewCleanupTask creates a new cleanup task.
func NewCleanupTask(db *database.DatabaseConnection, c cache.Cache) *CleanupTask {
    cleanLog.Info("Creating CleanupTask")
    return &CleanupTask{DB: db, Cache: c}
}

// CleanExpiredSessions removes expired sessions from the database.
func (t *CleanupTask) CleanExpiredSessions() (int, error) {
    cleanLog.Info("Cleaning expired sessions")
    results, err := t.DB.ExecuteQuery("DELETE FROM sessions WHERE expires_at < NOW()")
    if err != nil {
        cleanLog.Error("Failed to clean sessions: %v", err)
        return 0, err
    }
    count := len(results)
    cleanLog.Info("Cleaned %d expired sessions", count)
    return count, nil
}

// CleanExpiredTokens removes expired tokens.
func (t *CleanupTask) CleanExpiredTokens() (int, error) {
    cleanLog.Info("Cleaning expired tokens")
    results, err := t.DB.ExecuteQuery("DELETE FROM tokens WHERE expires_at < NOW()")
    if err != nil {
        cleanLog.Error("Failed to clean tokens: %v", err)
        return 0, err
    }
    count := len(results)
    cleanLog.Info("Cleaned %d expired tokens", count)
    return count, nil
}

// ClearCache flushes the entire cache.
func (t *CleanupTask) ClearCache() error {
    cleanLog.Info("Clearing cache")
    return t.Cache.Clear()
}

// Execute runs all cleanup tasks.
func (t *CleanupTask) Execute() error {
    cleanLog.Info("Executing cleanup task")
    sessions, _ := t.CleanExpiredSessions()
    tokens, _ := t.CleanExpiredTokens()
    _ = t.ClearCache()
    cleanLog.Info("Cleanup complete: %d sessions, %d tokens removed", sessions, tokens)
    return nil
}
