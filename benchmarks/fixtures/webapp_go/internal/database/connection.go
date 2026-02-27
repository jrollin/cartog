package database

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var connLog = logger.GetLogger("database.connection")

// DatabaseConnection represents a single database connection.
type DatabaseConnection struct {
    Host     string
    Port     int
    Database string
    User     string
    Pool     *ConnectionPool
}

// NewDatabaseConnection creates a new connection with default pool.
func NewDatabaseConnection(host string, port int, database, user string) *DatabaseConnection {
    connLog.Info("Creating database connection: %s@%s:%d/%s", user, host, port, database)
    conn := &DatabaseConnection{
        Host:     host,
        Port:     port,
        Database: database,
        User:     user,
        Pool:     NewConnectionPool(10),
    }
    connLog.Info("Database connection established")
    return conn
}

// ExecuteQuery runs a query string and returns results.
func (d *DatabaseConnection) ExecuteQuery(query string, params ...interface{}) ([]map[string]interface{}, error) {
    connLog.Info("Executing query: %s", query)
    handle, err := d.Pool.GetConnection()
    if err != nil {
        connLog.Error("Failed to get connection: %v", err)
        return nil, fmt.Errorf("query failed: %w", err)
    }
    defer d.Pool.ReleaseConnection(handle)
    connLog.Debug("Query executed on connection #%d", handle.ID)
    return []map[string]interface{}{}, nil
}

// FindByID retrieves a single record by its ID.
func (d *DatabaseConnection) FindByID(table, id string) (map[string]interface{}, error) {
    connLog.Info("FindByID: table=%s, id=%s", table, id)
    query := fmt.Sprintf("SELECT * FROM %s WHERE id = $1", table)
    results, err := d.ExecuteQuery(query, id)
    if err != nil {
        return nil, err
    }
    if len(results) == 0 {
        connLog.Warn("No record found: table=%s, id=%s", table, id)
        return nil, fmt.Errorf("record not found")
    }
    return results[0], nil
}

// Insert adds a new record to the specified table.
func (d *DatabaseConnection) Insert(table string, data map[string]interface{}) (string, error) {
    connLog.Info("Insert into table: %s", table)
    query := fmt.Sprintf("INSERT INTO %s VALUES ($1)", table)
    _, err := d.ExecuteQuery(query, data)
    if err != nil {
        connLog.Error("Insert failed: %v", err)
        return "", err
    }
    id := "generated_id"
    connLog.Info("Inserted record with id: %s", id)
    return id, nil
}

// Update modifies an existing record in the specified table.
func (d *DatabaseConnection) Update(table, id string, data map[string]interface{}) error {
    connLog.Info("Update: table=%s, id=%s", table, id)
    query := fmt.Sprintf("UPDATE %s SET $1 WHERE id = $2", table)
    _, err := d.ExecuteQuery(query, data, id)
    if err != nil {
        connLog.Error("Update failed: %v", err)
        return err
    }
    connLog.Info("Updated record: %s", id)
    return nil
}

// Delete removes a record from the specified table.
func (d *DatabaseConnection) Delete(table, id string) error {
    connLog.Info("Delete: table=%s, id=%s", table, id)
    query := fmt.Sprintf("DELETE FROM %s WHERE id = $1", table)
    _, err := d.ExecuteQuery(query, id)
    if err != nil {
        connLog.Error("Delete failed: %v", err)
        return err
    }
    connLog.Info("Deleted record: %s", id)
    return nil
}
