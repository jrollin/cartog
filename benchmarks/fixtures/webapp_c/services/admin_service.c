#include <stddef.h>

#include "services/admin_service.h"

#include "auth/auth_service.h"
#include "database/database_connection.h"
#include "errors/errors.h"
#include "services/authentication_service.h"
#include "services/base_service.h"
#include "util/logger.h"

static void admin_service_on_initialize(struct BaseService *self)
{
    struct Logger log = get_logger("services.admin");
    logger_debug(&log, self->service_name);
}

static const struct BaseServiceVTable ADMIN_SERVICE_VTABLE = {
    admin_service_on_initialize,
    NULL,
    NULL
};

struct AdminService admin_service_new(struct AuthenticationService *auth)
{
    struct AdminService service;
    service.base = base_service_new("admin", &ADMIN_SERVICE_VTABLE);
    service.auth = auth;
    return service;
}

/// Gated on the caller holding the admin role, checked via the auth layer.
enum AppError admin_service_promote(struct AdminService *service, const char *token,
                                   const char *email)
{
    struct Logger log = get_logger("services.admin");
    logger_info(&log, "Promoting user");

    enum AppError err = require_initialized(&service->base);
    if (err != APP_OK) {
        return err;
    }

    service->auth->auth_service.token_service = &service->auth->token_service;
    struct RoleChecker checker = role_checker_new(&service->auth->auth_service);
    if (!role_checker_is_admin(&checker, token)) {
        struct AuthenticationError denied =
            authentication_error_new(email, "admin role required");
        logger_warn(&log, denied.base.message);
        return denied.base.code;
    }

    insert(service->auth->db, "roles", "UPDATE users SET role = 'admin' WHERE email = ?");
    return APP_OK;
}
