#ifndef ROUTES_AUTH_ROUTES_H
#define ROUTES_AUTH_ROUTES_H

#include "errors/errors.h"

/// Request/response pair the login route hands to the service layer.
struct LoginHandler {
    const char *email;
    const char *password;
    const char *issued_token;
    enum AppError status;
};

/// HTTP route handlers for the authentication endpoints.
struct AuthRoutes {
    const char *prefix;
};

struct AuthRoutes auth_routes_new(const char *prefix);

/// POST /login: builds the service graph, then delegates to authenticate.
const char *login_handler(struct AuthRoutes *routes, const char *email, const char *password);
enum AppError logout_handler(struct AuthRoutes *routes, const char *token);

#endif /* ROUTES_AUTH_ROUTES_H */
