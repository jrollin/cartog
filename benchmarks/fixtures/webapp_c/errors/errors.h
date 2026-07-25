#ifndef ERRORS_ERRORS_H
#define ERRORS_ERRORS_H

/// Application-wide error codes. C has no exceptions, so every fallible
/// function returns one of these and writes its payload through an out-param.
enum AppError {
    APP_OK = 0,
    APP_ERROR_GENERIC = 1,
    APP_ERROR_VALIDATION = 2,
    APP_ERROR_AUTHENTICATION = 3,
    APP_ERROR_TOKEN = 4,
    APP_ERROR_TOKEN_EXPIRED = 5
};

/// Base error payload. The specific error structs below embed it as their
/// first member, the same struct-embedding idiom used for BaseService.
struct AppErrorInfo {
    enum AppError code;
    const char *message;
};

/// Raised when user-supplied input fails a validator.
struct ValidationError {
    struct AppErrorInfo base;
    const char *field;
};

/// Raised when credentials are missing or wrong.
struct AuthenticationError {
    struct AppErrorInfo base;
    const char *email;
};

/// Raised for a malformed or revoked token.
struct TokenError {
    struct AppErrorInfo base;
    const char *token;
};

/// Raised when a token parsed cleanly but is past its expiry.
struct ExpiredTokenError {
    struct TokenError base;
    long expired_at;
};

struct AppErrorInfo app_error_new(enum AppError code, const char *message);
struct ValidationError validation_error_new(const char *field, const char *message);
struct AuthenticationError authentication_error_new(const char *email, const char *message);
struct TokenError token_error_new(const char *token, const char *message);
struct ExpiredTokenError expired_token_error_new(const char *token, long expired_at);
const char *app_error_name(enum AppError code);

#endif /* ERRORS_ERRORS_H */
