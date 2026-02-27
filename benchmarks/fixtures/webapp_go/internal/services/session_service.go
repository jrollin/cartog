package services

import (
    "webapp_go/internal/database"
    "webapp_go/internal/models"
    "webapp_go/pkg/logger"
)

var sessSvcLog = logger.GetLogger("services.session")

// SessionService manages user sessions.
type SessionService struct {
    BaseServiceImpl
    DB *database.DatabaseConnection
}

// NewSessionService creates a new session service.
func NewSessionService(db *database.DatabaseConnection) *SessionService {
    sessSvcLog.Info("Creating SessionService")
    return &SessionService{
        BaseServiceImpl: BaseServiceImpl{
            ServiceName:    "session",
            ServiceVersion: "1.0",
        },
        DB: db,
    }
}

// Create starts a new session.
func (s *SessionService) Create(userID, token, ip, userAgent string) *models.Session {
    sessSvcLog.Info("Creating session for user: %s", userID)
    session := models.NewSession(userID, token, ip, userAgent)
    s.DB.Insert("sessions", map[string]interface{}{
        "user_id": userID,
        "token":   token,
    })
    return session
}

// Invalidate revokes a session.
func (s *SessionService) Invalidate(sessionID string) error {
    sessSvcLog.Info("Invalidating session: %s", sessionID)
    return s.DB.Delete("sessions", sessionID)
}

// InvalidateAll revokes all sessions for a user.
func (s *SessionService) InvalidateAll(userID string) error {
    sessSvcLog.Info("Invalidating all sessions for user: %s", userID)
    _, err := s.DB.ExecuteQuery("DELETE FROM sessions WHERE user_id = $1", userID)
    return err
}

// FindByToken looks up a session by token.
func (s *SessionService) FindByToken(token string) (*models.Session, error) {
    sessSvcLog.Info("Finding session by token")
    results, err := s.DB.ExecuteQuery("SELECT * FROM sessions WHERE token = $1", token)
    if err != nil {
        return nil, err
    }
    if len(results) == 0 {
        return nil, nil
    }
    return &models.Session{Token: token, Status: models.SessionActive}, nil
}
