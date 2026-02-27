package services

import (
    "fmt"

    "webapp_go/internal/database"
    "webapp_go/internal/models"
    "webapp_go/pkg/logger"
)

var userSvcLog = logger.GetLogger("services.user")

// UserService manages user CRUD operations.
type UserService struct {
    BaseServiceImpl
    DB *database.DatabaseConnection
}

// NewUserService creates a new user service.
func NewUserService(db *database.DatabaseConnection) *UserService {
    userSvcLog.Info("Creating UserService")
    return &UserService{
        BaseServiceImpl: BaseServiceImpl{
            ServiceName:    "user",
            ServiceVersion: "1.0",
        },
        DB: db,
    }
}

// Create adds a new user.
func (s *UserService) Create(email, name, password string) (*models.User, error) {
    userSvcLog.Info("Creating user: %s", email)
    user := models.NewUser(email, name, password)
    errs := user.Validate()
    if len(errs) > 0 {
        userSvcLog.Warn("User validation failed: %v", errs)
        return nil, fmt.Errorf("validation failed: %v", errs)
    }
    _, err := s.DB.Insert("users", map[string]interface{}{"email": email, "name": name})
    if err != nil {
        userSvcLog.Error("Failed to insert user: %v", err)
        return nil, err
    }
    userSvcLog.Info("User created: %s", email)
    return user, nil
}

// FindByID looks up a user by ID.
func (s *UserService) FindByID(id string) (*models.User, error) {
    userSvcLog.Info("Finding user by ID: %s", id)
    _, err := s.DB.FindByID("users", id)
    if err != nil {
        return nil, err
    }
    return &models.User{ID: id}, nil
}

// Update modifies an existing user.
func (s *UserService) Update(id string, data map[string]interface{}) error {
    userSvcLog.Info("Updating user: %s", id)
    return s.DB.Update("users", id, data)
}

// Delete removes a user.
func (s *UserService) Delete(id string) error {
    userSvcLog.Info("Deleting user: %s", id)
    return s.DB.Delete("users", id)
}

// Deactivate disables a user account.
func (s *UserService) Deactivate(id string) error {
    userSvcLog.Info("Deactivating user: %s", id)
    return s.DB.Update("users", id, map[string]interface{}{"active": false})
}
