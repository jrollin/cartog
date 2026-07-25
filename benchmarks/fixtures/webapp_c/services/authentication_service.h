#ifndef SERVICES_AUTHENTICATION_SERVICE_H
#define SERVICES_AUTHENTICATION_SERVICE_H

#include "auth/auth_service.h"
#include "auth/token_service.h"
#include "database/database_connection.h"
#include "errors/errors.h"
#include "models/user.h"
#include "services/base_service.h"

/// Orchestrates the full authentication workflow.
///
/// Deep call chain hop 2:
/// handle_login -> authenticate -> login -> generate_token -> execute_query
///   -> get_connection
///
/// `base` first = the C struct-embedding stand-in for inheritance.
struct AuthenticationService {
    struct BaseService base;
    struct AuthService auth_service;
    struct TokenService token_service;
    struct DatabaseConnection *db;
};

/// Owns its AuthService/TokenService, so it takes the db it will hand down.
struct AuthenticationService authentication_service_new(struct DatabaseConnection *db);

/// Perform the full authentication flow. Returns NULL when it fails.
const char *authenticate(struct AuthenticationService *service, const char *email,
                         const char *password);
enum AppError authentication_service_logout(struct AuthenticationService *service,
                                            const char *token);
enum AppError authentication_service_current_user(struct AuthenticationService *service,
                                                  const char *token, struct User *out_user);

#endif /* SERVICES_AUTHENTICATION_SERVICE_H */
