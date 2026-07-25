#ifndef MIDDLEWARE_AUTH_MIDDLEWARE_H
#define MIDDLEWARE_AUTH_MIDDLEWARE_H

#include "auth/token_service.h"
#include "errors/errors.h"

/// Middleware that authenticates a request by validating its bearer token.
struct AuthMiddleware {
    struct TokenService *token_service;
};

struct AuthMiddleware auth_middleware_new(struct TokenService *token_service);
enum AppError auth_middleware_handle(struct AuthMiddleware *middleware, const char *token);

#endif /* MIDDLEWARE_AUTH_MIDDLEWARE_H */
