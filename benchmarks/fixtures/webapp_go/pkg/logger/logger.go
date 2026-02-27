package logger

import (
    "fmt"
    "time"
)

// LogLevel represents the severity of a log message.
type LogLevel int

const (
    DEBUG LogLevel = iota
    INFO
    WARN
    ERROR
    FATAL
)

// Logger provides structured logging for a named component.
type Logger struct {
    Name  string
    Level LogLevel
}

// GetLogger creates a new Logger instance for the given component name.
func GetLogger(name string) *Logger {
    return &Logger{
        Name:  name,
        Level: DEBUG,
    }
}

func (l *Logger) timestamp() string {
    return time.Now().Format("2006-01-02T15:04:05.000Z07:00")
}

// Info logs an informational message.
func (l *Logger) Info(msg string, args ...interface{}) {
    if l.Level <= INFO {
        formatted := fmt.Sprintf(msg, args...)
        fmt.Printf("[%s] INFO  [%s] %s\n", l.timestamp(), l.Name, formatted)
    }
}

// Error logs an error message.
func (l *Logger) Error(msg string, args ...interface{}) {
    if l.Level <= ERROR {
        formatted := fmt.Sprintf(msg, args...)
        fmt.Printf("[%s] ERROR [%s] %s\n", l.timestamp(), l.Name, formatted)
    }
}

// Warn logs a warning message.
func (l *Logger) Warn(msg string, args ...interface{}) {
    if l.Level <= WARN {
        formatted := fmt.Sprintf(msg, args...)
        fmt.Printf("[%s] WARN  [%s] %s\n", l.timestamp(), l.Name, formatted)
    }
}

// Debug logs a debug message.
func (l *Logger) Debug(msg string, args ...interface{}) {
    if l.Level <= DEBUG {
        formatted := fmt.Sprintf(msg, args...)
        fmt.Printf("[%s] DEBUG [%s] %s\n", l.timestamp(), l.Name, formatted)
    }
}

// SetLevel changes the minimum log level for this logger.
func (l *Logger) SetLevel(level LogLevel) {
    l.Level = level
}

// WithField returns a child logger with an added context field.
func (l *Logger) WithField(key, value string) *Logger {
    return &Logger{
        Name:  fmt.Sprintf("%s[%s=%s]", l.Name, key, value),
        Level: l.Level,
    }
}
