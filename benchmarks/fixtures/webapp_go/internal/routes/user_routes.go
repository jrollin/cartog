package routes

import (
    "fmt"

    "webapp_go/internal/database"
    "webapp_go/internal/services"
    "webapp_go/internal/validators"
    "webapp_go/pkg/logger"
)

var userRouteLog = logger.GetLogger("routes.user")

// UserHandler handles user CRUD requests.
func UserHandler(request map[string]interface{}) (map[string]interface{}, error) {
    userRouteLog.Info("User request received")

    action, _ := request["action"].(string)
    db := database.NewDatabaseConnection("localhost", 5432, "app", "user")
    userSvc := services.NewUserService(db)

    switch action {
    case "create":
        userRouteLog.Info("Creating user")
        email, _ := request["email"].(string)
        name, _ := request["name"].(string)
        password, _ := request["password"].(string)

        validator := validators.NewUserValidator()
        errs := validator.Validate(map[string]string{
            "email": email, "name": name, "password": password,
        })
        if len(errs) > 0 {
            userRouteLog.Warn("Validation failed")
            return nil, fmt.Errorf("validation failed")
        }

        user, err := userSvc.Create(email, name, password)
        if err != nil {
            return nil, err
        }
        return map[string]interface{}{"user_id": user.ID}, nil

    case "get":
        userRouteLog.Info("Getting user")
        id, _ := request["id"].(string)
        user, err := userSvc.FindByID(id)
        if err != nil {
            return nil, err
        }
        return map[string]interface{}{"user": user}, nil

    case "delete":
        userRouteLog.Info("Deleting user")
        id, _ := request["id"].(string)
        err := userSvc.Delete(id)
        if err != nil {
            return nil, err
        }
        return map[string]interface{}{"status": "deleted"}, nil

    default:
        return nil, fmt.Errorf("unknown action: %s", action)
    }
}
