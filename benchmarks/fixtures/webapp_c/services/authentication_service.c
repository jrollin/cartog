#include <stddef.h>

#include "services/authentication_service.h"

#include "auth/auth_service.h"
#include "auth/token_service.h"
#include "database/database_connection.h"
#include "errors/errors.h"
#include "models/user.h"
#include "services/base_service.h"
#include "util/logger.h"

static void authentication_service_on_initialize(struct BaseService *self)
{
    struct Logger log = get_logger("services.authentication");
    logger_debug(&log, self->service_name);
}

static const char *authentication_service_describe(const struct BaseService *self)
{
    (void)self;
    return "authentication service";
}

static const struct BaseServiceVTable AUTHENTICATION_SERVICE_VTABLE = {
    authentication_service_on_initialize,
    NULL,
    authentication_service_describe
};

struct AuthenticationService authentication_service_new(struct DatabaseConnection *db)
{
    struct Logger log = get_logger("services.authentication");
    logger_info(&log, "Creating AuthenticationService");

    struct AuthenticationService service;
    service.base = base_service_new("authentication", &AUTHENTICATION_SERVICE_VTABLE);
    service.db = db;
    service.token_service = token_service_new(db);
    service.auth_service = auth_service_new(&service.token_service);
    return service;
}

/// Hop 2 of the deep chain. Re-points auth_service at this object's own
/// token_service: the struct was copied out of the constructor, so the
/// interior pointer taken there no longer refers to this instance.
const char *authenticate(struct AuthenticationService *service, const char *email,
                         const char *password)
{
    struct Logger log = get_logger("services.authentication");

    if (require_initialized(&service->base) != APP_OK) {
        logger_warn(&log, "Authenticating on an uninitialized service");
    }
    service->auth_service.token_service = &service->token_service;

    logger_info(&log, "Authenticating user");
    const char *token = login(&service->auth_service, email, password);
    if (token == NULL) {
        logger_warn(&log, "Authentication failed");
        return NULL;
    }

    insert(service->db, "sessions", "INSERT INTO sessions (token, email) VALUES (?, ?)");

    logger_info(&log, "Authentication successful");
    return token;
}

enum AppError authentication_service_logout(struct AuthenticationService *service,
                                            const char *token)
{
    struct Logger log = get_logger("services.authentication");
    logger_info(&log, "Logging out");
    service->auth_service.token_service = &service->token_service;
    return logout(&service->auth_service, token);
}

enum AppError authentication_service_current_user(struct AuthenticationService *service,
                                                  const char *token, struct User *out_user)
{
    struct Logger log = get_logger("services.authentication");
    logger_info(&log, "Getting current user");
    service->auth_service.token_service = &service->token_service;
    return get_current_user(&service->auth_service, token, out_user);
}
