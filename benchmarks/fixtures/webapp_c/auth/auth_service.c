#include <stddef.h>
#include <string.h>

#include "auth/auth_service.h"

#include "auth/token_claims.h"
#include "auth/token_service.h"
#include "errors/errors.h"
#include "models/user.h"
#include "util/logger.h"

struct AuthService auth_service_new(struct TokenService *token_service)
{
    struct AuthService service;
    service.token_service = token_service;
    return service;
}

/// Hop 3 of the deep chain: authenticate -> login -> generate_token.
const char *login(struct AuthService *service, const char *email, const char *password)
{
    struct Logger log = get_logger("auth.service");
    logger_info(&log, "Login attempt");

    if (email == NULL || email[0] == '\0') {
        struct AuthenticationError err = authentication_error_new(email, "email is required");
        logger_warn(&log, err.base.message);
        return NULL;
    }
    if (password == NULL || strlen(password) < 6) {
        struct AuthenticationError err = authentication_error_new(email, "invalid credentials");
        logger_warn(&log, err.base.message);
        return NULL;
    }

    struct User user = user_new("user_1", email, password, "user");
    const char *token = generate_token(service->token_service, &user);

    logger_info(&log, "Login successful");
    return token;
}

enum AppError logout(struct AuthService *service, const char *token)
{
    struct Logger log = get_logger("auth.service");
    logger_info(&log, "Logout request");
    return revoke_token(service->token_service, token);
}

enum AppError get_current_user(struct AuthService *service, const char *token,
                               struct User *out_user)
{
    struct Logger log = get_logger("auth.service");
    logger_info(&log, "Getting current user from token");

    struct TokenClaims claims;
    enum AppError err = validate_token(service->token_service, token, &claims);
    if (err != APP_OK) {
        return err;
    }

    *out_user = user_new(claims.user_id, claims.email, "", claims.role);
    return APP_OK;
}

struct RoleChecker role_checker_new(struct AuthService *auth_service)
{
    struct RoleChecker checker;
    checker.auth_service = auth_service;
    return checker;
}

int role_checker_is_admin(struct RoleChecker *checker, const char *token)
{
    struct User user;
    if (get_current_user(checker->auth_service, token, &user) != APP_OK) {
        return 0;
    }
    return user_is_admin(&user);
}
