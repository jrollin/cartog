#include <stddef.h>

#include "errors/errors.h"

/// Build the base error payload every specific error embeds.
struct AppErrorInfo app_error_new(enum AppError code, const char *message)
{
    struct AppErrorInfo info;
    info.code = code;
    info.message = message;
    return info;
}

struct ValidationError validation_error_new(const char *field, const char *message)
{
    struct ValidationError err;
    err.base = app_error_new(APP_ERROR_VALIDATION, message);
    err.field = field;
    return err;
}

struct AuthenticationError authentication_error_new(const char *email, const char *message)
{
    struct AuthenticationError err;
    err.base = app_error_new(APP_ERROR_AUTHENTICATION, message);
    err.email = email;
    return err;
}

struct TokenError token_error_new(const char *token, const char *message)
{
    struct TokenError err;
    err.base = app_error_new(APP_ERROR_TOKEN, message);
    err.token = token;
    return err;
}

/// An expired token embeds TokenError, so it is usable wherever a TokenError is.
struct ExpiredTokenError expired_token_error_new(const char *token, long expired_at)
{
    struct ExpiredTokenError err;
    err.base = token_error_new(token, "token expired");
    err.base.base.code = APP_ERROR_TOKEN_EXPIRED;
    err.expired_at = expired_at;
    return err;
}

const char *app_error_name(enum AppError code)
{
    switch (code) {
    case APP_OK:
        return "ok";
    case APP_ERROR_VALIDATION:
        return "validation_error";
    case APP_ERROR_AUTHENTICATION:
        return "authentication_error";
    case APP_ERROR_TOKEN:
        return "token_error";
    case APP_ERROR_TOKEN_EXPIRED:
        return "expired_token_error";
    case APP_ERROR_GENERIC:
    default:
        return "app_error";
    }
}
