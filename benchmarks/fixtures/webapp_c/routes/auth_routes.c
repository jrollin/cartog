#include <stddef.h>

#include "routes/auth_routes.h"

#include "database/database_connection.h"
#include "services/authentication_service.h"
#include "util/logger.h"

struct AuthRoutes auth_routes_new(const char *prefix)
{
    struct AuthRoutes routes;
    routes.prefix = prefix;
    return routes;
}

/// POST /login. Hop between the controller and the service layer.
const char *login_handler(struct AuthRoutes *routes, const char *email, const char *password)
{
    struct Logger log = get_logger("routes.auth");
    logger_info(&log, "Handling login request");
    (void)routes;

    struct LoginHandler request;
    request.email = email;
    request.password = password;
    request.issued_token = NULL;
    request.status = APP_OK;

    struct DatabaseConnection db = database_connection_new("localhost", 5432, "app");
    struct AuthenticationService service = authentication_service_new(&db);
    base_service_initialize(&service.base);

    request.issued_token = authenticate(&service, request.email, request.password);
    if (request.issued_token == NULL) {
        request.status = APP_ERROR_AUTHENTICATION;
        logger_warn(&log, app_error_name(request.status));
    }
    return request.issued_token;
}

enum AppError logout_handler(struct AuthRoutes *routes, const char *token)
{
    struct Logger log = get_logger("routes.auth");
    logger_info(&log, "Handling logout request");
    (void)routes;

    struct DatabaseConnection db = database_connection_new("localhost", 5432, "app");
    struct AuthenticationService service = authentication_service_new(&db);
    base_service_initialize(&service.base);

    return authentication_service_logout(&service, token);
}
