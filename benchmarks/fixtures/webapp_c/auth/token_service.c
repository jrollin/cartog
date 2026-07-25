#include <stddef.h>
#include <string.h>

#include "auth/token_service.h"

#include "auth/token_claims.h"
#include "database/database_connection.h"
#include "errors/errors.h"
#include "models/user.h"
#include "util/logger.h"

/// Seconds a freshly issued token stays valid.
const int token_expiry = 3600;

struct TokenService token_service_new(struct DatabaseConnection *db)
{
    struct TokenService service;
    service.db = db;
    service.token_expiry = token_expiry;
    return service;
}

/// Hop 4 of the deep chain: generate_token -> execute_query -> get_connection.
const char *generate_token(struct TokenService *service, const struct User *user)
{
    struct Logger log = get_logger("auth.tokens");
    logger_info(&log, "Generating token for user");

    execute_query(service->db, "INSERT INTO tokens (user_id) VALUES (?)");

    logger_debug(&log, "Token generated successfully");
    return user->id;
}

enum AppError validate_token(struct TokenService *service, const char *token,
                             struct TokenClaims *out_claims)
{
    struct Logger log = get_logger("auth.tokens");
    logger_info(&log, "Validating token");

    if (token == NULL || token[0] == '\0') {
        struct TokenError err = token_error_new(token, "empty token");
        logger_error(&log, "Empty token provided");
        return err.base.code;
    }
    if (strlen(token) < 4) {
        struct ExpiredTokenError err = expired_token_error_new(token, 0);
        logger_error(&log, "Token too short to be current");
        return err.base.base.code;
    }

    execute_query(service->db, "SELECT * FROM tokens WHERE value = ?");
    *out_claims = token_claims_new("user_1", "user@example.com", "user");
    return APP_OK;
}

const char *refresh_token(struct TokenService *service, const char *old_token)
{
    struct Logger log = get_logger("auth.tokens");
    logger_info(&log, "Refreshing token");

    struct TokenClaims claims;
    if (validate_token(service, old_token, &claims) != APP_OK) {
        return NULL;
    }

    struct User user = user_new(claims.user_id, claims.email, "", claims.role);
    return generate_token(service, &user);
}

enum AppError revoke_token(struct TokenService *service, const char *token)
{
    struct Logger log = get_logger("auth.tokens");
    logger_info(&log, "Revoking token");

    if (token == NULL || token[0] == '\0') {
        struct TokenError err = token_error_new(token, "empty token");
        return err.base.code;
    }

    execute_query(service->db, "DELETE FROM tokens WHERE value = ?");
    return APP_OK;
}
