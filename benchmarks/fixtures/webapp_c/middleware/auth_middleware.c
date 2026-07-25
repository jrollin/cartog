#include "middleware/auth_middleware.h"

#include "auth/token_claims.h"
#include "auth/token_service.h"
#include "errors/errors.h"
#include "util/logger.h"

struct AuthMiddleware auth_middleware_new(struct TokenService *token_service)
{
    struct AuthMiddleware middleware;
    middleware.token_service = token_service;
    return middleware;
}

/// Rejects the request unless the token validates.
enum AppError auth_middleware_handle(struct AuthMiddleware *middleware, const char *token)
{
    struct Logger log = get_logger("middleware.auth");
    logger_info(&log, "Authenticating request");

    struct TokenClaims claims;
    enum AppError err = validate_token(middleware->token_service, token, &claims);
    if (err != APP_OK) {
        logger_warn(&log, app_error_name(err));
        return err;
    }

    logger_debug(&log, claims.email);
    return APP_OK;
}
