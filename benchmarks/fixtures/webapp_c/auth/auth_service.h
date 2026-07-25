#ifndef AUTH_AUTH_SERVICE_H
#define AUTH_AUTH_SERVICE_H

#include "auth/token_service.h"
#include "errors/errors.h"
#include "models/user.h"

/// Handles user authentication flows on top of the token service.
struct AuthService {
    struct TokenService *token_service;
};

/// Role check layered on top of the auth service. The BaseService-embedding
/// AdminService in services/ delegates its role gate through this.
struct RoleChecker {
    struct AuthService *auth_service;
};

struct AuthService auth_service_new(struct TokenService *token_service);

/// Hop 3 of the deep chain: login -> generate_token -> execute_query.
const char *login(struct AuthService *service, const char *email, const char *password);
enum AppError logout(struct AuthService *service, const char *token);
enum AppError get_current_user(struct AuthService *service, const char *token,
                               struct User *out_user);

struct RoleChecker role_checker_new(struct AuthService *auth_service);
int role_checker_is_admin(struct RoleChecker *checker, const char *token);

#endif /* AUTH_AUTH_SERVICE_H */
