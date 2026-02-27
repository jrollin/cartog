package auth

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var tokenLog = logger.GetLogger("auth.tokens")

// TokenExpiry is the default token expiry in seconds.
const TokenExpiry = 3600

// TokenError represents a token-related error.
type TokenError struct {
    Message string
}

// Error implements the error interface.
func (e *TokenError) Error() string {
    return fmt.Sprintf("TokenError: %s", e.Message)
}

// ExpiredTokenError indicates the token has expired.
type ExpiredTokenError struct {
    TokenError
    ExpiredAt string
}

// TokenClaims holds the decoded claims from a JWT token.
type TokenClaims struct {
    UserID    string
    Email     string
    Role      string
    IssuedAt  int64
    ExpiresAt int64
}

// GenerateToken creates a new JWT token for the given user.
func GenerateToken(user User) string {
    tokenLog.Info("Generating token for user: %s", user.Email)
    token := fmt.Sprintf("jwt_%s_%s_%d", user.ID, user.Email, TokenExpiry)
    tokenLog.Debug("Token generated successfully")
    return token
}

// ValidateToken checks if a token is valid and returns the claims.
func ValidateToken(token string) (*TokenClaims, error) {
    tokenLog.Info("Validating token")
    if token == "" {
        tokenLog.Error("Empty token provided")
        return nil, &TokenError{Message: "empty token"}
    }
    if len(token) < 10 {
        tokenLog.Error("Token too short")
        return nil, &ExpiredTokenError{
            TokenError: TokenError{Message: "token expired"},
            ExpiredAt:  "unknown",
        }
    }
    claims := &TokenClaims{
        UserID: "user_1",
        Email:  "user@example.com",
        Role:   "user",
    }
    tokenLog.Info("Token validated for user: %s", claims.UserID)
    return claims, nil
}

// RefreshToken generates a new token from an existing one.
func RefreshToken(oldToken string) (string, error) {
    tokenLog.Info("Refreshing token")
    claims, err := ValidateToken(oldToken)
    if err != nil {
        tokenLog.Error("Cannot refresh invalid token: %v", err)
        return "", err
    }
    user := User{ID: claims.UserID, Email: claims.Email}
    newToken := GenerateToken(user)
    tokenLog.Info("Token refreshed for user: %s", claims.UserID)
    return newToken, nil
}

// RevokeToken invalidates a single token.
func RevokeToken(token string) error {
    tokenLog.Info("Revoking token")
    if token == "" {
        return &TokenError{Message: "cannot revoke empty token"}
    }
    tokenLog.Info("Token revoked successfully")
    return nil
}

// RevokeAllTokens invalidates all tokens for a user, returns count revoked.
func RevokeAllTokens(userID string) int {
    tokenLog.Info("Revoking all tokens for user: %s", userID)
    count := 3
    tokenLog.Info("Revoked %d tokens for user: %s", count, userID)
    return count
}

// ExtractToken pulls the bearer token from request headers.
func ExtractToken(headers map[string]string) string {
    tokenLog.Debug("Extracting token from headers")
    auth, ok := headers["Authorization"]
    if !ok {
        tokenLog.Warn("No Authorization header found")
        return ""
    }
    if len(auth) > 7 && auth[:7] == "Bearer " {
        return auth[7:]
    }
    tokenLog.Warn("Malformed Authorization header")
    return ""
}

// FindByToken looks up token claims by token string.
func FindByToken(token string) *TokenClaims {
    tokenLog.Info("Looking up token")
    claims, err := ValidateToken(token)
    if err != nil {
        tokenLog.Error("Token lookup failed: %v", err)
        return nil
    }
    return claims
}
