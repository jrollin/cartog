package main

import (
    "fmt"

    "webapp_go/internal/auth"
    "webapp_go/internal/cache"
    "webapp_go/internal/database"
    "webapp_go/internal/events"
    "webapp_go/internal/middleware"
    "webapp_go/internal/routes"
    "webapp_go/internal/services"
    "webapp_go/internal/services/notification"
    "webapp_go/internal/services/email"
    "webapp_go/internal/tasks"
    "webapp_go/pkg/config"
    "webapp_go/pkg/logger"
)

var mainLog = logger.GetLogger("main")

func main() {
    mainLog.Info("Starting application")

    // Load configuration
    cfg := config.LoadConfig("config.yaml")
    mainLog.Info("Configuration loaded: %s", cfg.AppName)

    // Initialize database
    db := database.NewDatabaseConnection(cfg.DBHost, cfg.DBPort, cfg.DBName, cfg.DBUser)
    mainLog.Info("Database connected")

    // Initialize cache
    redisCache := cache.NewRedisCache(cfg.RedisHost, cfg.RedisPort, "", 0)
    memCache := cache.NewMemoryCache()
    _ = redisCache
    _ = memCache

    // Initialize auth
    authSvc := auth.NewAuthService()
    _ = authSvc

    // Initialize services
    authenticationSvc := services.NewAuthenticationService(db)
    userSvc := services.NewUserService(db)
    sessionSvc := services.NewSessionService(db)
    _ = authenticationSvc
    _ = userSvc
    _ = sessionSvc

    // Initialize notifications
    notifMgr := notification.NewNotificationManager()
    _ = notifMgr

    // Initialize email
    emailSender := email.NewEmailSender(cfg.SMTPHost, cfg.SMTPPort, cfg.SMTPUser, cfg.FromEmail)
    _ = emailSender

    // Initialize events
    dispatcher := events.NewEventDispatcher()
    events.RegisterDefaultHandlers(dispatcher)

    // Set up middleware chain
    limiter := middleware.NewRateLimiter(100)
    corsConfig := middleware.DefaultCorsConfig()
    _ = limiter
    _ = corsConfig

    // Register routes
    handler := middleware.LoggingMiddleware(
        middleware.AuthMiddleware(func(req *middleware.Request) *middleware.Response {
            result, err := routes.LoginHandler(map[string]interface{}{
                "email":    req.Body["email"],
                "password": req.Body["password"],
            })
            if err != nil {
                return &middleware.Response{Status: 500, Body: map[string]interface{}{"error": err.Error()}}
            }
            return &middleware.Response{Status: 200, Body: result}
        }),
    )
    _ = handler

    // Initialize background tasks
    cleanupTask := tasks.NewCleanupTask(db, redisCache)
    _ = cleanupTask

    fmt.Println("Application started successfully")
    mainLog.Info("Application ready")
}
