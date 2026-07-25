#ifndef AUTH_TOKEN_SERVICE_H
#define AUTH_TOKEN_SERVICE_H

#include "auth/token_claims.h"
#include "database/database_connection.h"
#include "errors/errors.h"
#include "models/user.h"

/// Issues, validates, and revokes authentication tokens.
struct TokenService {
    struct DatabaseConnection *db;
    int token_expiry;
};

/// Seconds a freshly issued token stays valid.
extern const int token_expiry;

struct TokenService token_service_new(struct DatabaseConnection *db);

/// Issue a token for a user and persist it through the database layer.
const char *generate_token(struct TokenService *service, const struct User *user);

/// Validate a token, writing its claims to `out_claims` on success.
enum AppError validate_token(struct TokenService *service, const char *token,
                             struct TokenClaims *out_claims);

/// Validate then re-issue a token.
const char *refresh_token(struct TokenService *service, const char *old_token);

/// Invalidate a token so later validations reject it.
enum AppError revoke_token(struct TokenService *service, const char *token);

#endif /* AUTH_TOKEN_SERVICE_H */
