#include <stddef.h>

#include "services/user_service.h"

#include "database/database_connection.h"
#include "errors/errors.h"
#include "services/base_service.h"
#include "util/logger.h"

static void user_service_on_initialize(struct BaseService *self)
{
    struct Logger log = get_logger("services.user");
    logger_debug(&log, self->service_name);
}

static const char *user_service_describe(const struct BaseService *self)
{
    (void)self;
    return "user service";
}

static const struct BaseServiceVTable USER_SERVICE_VTABLE = {
    user_service_on_initialize,
    NULL,
    user_service_describe
};

struct UserService user_service_new(struct DatabaseConnection *db)
{
    struct UserService service;
    service.base = base_service_new("user", &USER_SERVICE_VTABLE);
    service.db = db;
    return service;
}

enum AppError user_service_create_user(struct UserService *service, const char *email)
{
    struct Logger log = get_logger("services.user");
    logger_info(&log, email);

    enum AppError err = require_initialized(&service->base);
    if (err != APP_OK) {
        return err;
    }

    insert(service->db, "users", "INSERT INTO users (email) VALUES (?)");
    return APP_OK;
}
