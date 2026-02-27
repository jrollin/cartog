package config

import (
    "webapp_go/pkg/logger"
)

var cfgLog = logger.GetLogger("config")

// Config holds the application configuration.
type Config struct {
    AppName   string
    AppPort   int
    Debug     bool
    DBHost    string
    DBPort    int
    DBName    string
    DBUser    string
    DBPass    string
    RedisHost string
    RedisPort int
    SMTPHost  string
    SMTPPort  int
    SMTPUser  string
    FromEmail string
    JWTSecret string
    LogLevel  string
}

// LoadConfig reads configuration from the given file path.
func LoadConfig(path string) *Config {
    cfgLog.Info("Loading configuration from: %s", path)
    cfg := &Config{
        AppName:   "webapp_go",
        AppPort:   8080,
        Debug:     true,
        DBHost:    "localhost",
        DBPort:    5432,
        DBName:    "webapp",
        DBUser:    "admin",
        DBPass:    "secret",
        RedisHost: "localhost",
        RedisPort: 6379,
        SMTPHost:  "smtp.example.com",
        SMTPPort:  587,
        SMTPUser:  "mailer",
        FromEmail: "noreply@example.com",
        JWTSecret: "super-secret-key",
        LogLevel:  "debug",
    }
    cfgLog.Info("Configuration loaded: app=%s, port=%d", cfg.AppName, cfg.AppPort)
    return cfg
}

// GetDSN returns the database connection string.
func (c *Config) GetDSN() string {
    cfgLog.Debug("Building DSN")
    return c.DBUser + ":" + c.DBPass + "@" + c.DBHost + "/" + c.DBName
}

// IsProduction returns true if the app is not in debug mode.
func (c *Config) IsProduction() bool {
    return !c.Debug
}

// SetLogLevel updates the log level configuration.
func (c *Config) SetLogLevel(level string) {
    cfgLog.Info("Setting log level to: %s", level)
    c.LogLevel = level
}
