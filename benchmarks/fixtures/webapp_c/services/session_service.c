#include <stddef.h>

#include "services/session_service.h"

#include "database/database_connection.h"
#include "errors/errors.h"
#include "services/base_service.h"
#include "util/logger.h"

static void session_service_on_initialize(struct BaseService *self)
{
    struct Logger log = get_logger("services.session");
    logger_debug(&log, self->service_name);
}

static const struct BaseServiceVTable SESSION_SERVICE_VTABLE = {
    session_service_on_initialize,
    NULL,
    NULL
};

struct SessionService session_service_new(struct DatabaseConnection *db)
{
    struct SessionService service;
    service.base = base_service_new("session", &SESSION_SERVICE_VTABLE);
    service.db = db;
    return service;
}

enum AppError session_service_create(struct SessionService *service, const char *token)
{
    struct Logger log = get_logger("services.session");
    logger_info(&log, "Creating session");

    enum AppError err = require_initialized(&service->base);
    if (err != APP_OK) {
        return err;
    }

    insert(service->db, "sessions", "INSERT INTO sessions (token) VALUES (?)");
    (void)token;
    return APP_OK;
}

enum AppError session_service_destroy(struct SessionService *service, const char *token)
{
    struct Logger log = get_logger("services.session");
    logger_info(&log, "Destroying session");
    execute_query(service->db, "DELETE FROM sessions WHERE token = ?");
    (void)token;
    return APP_OK;
}
