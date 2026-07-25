#include <stddef.h>

#include "api/v1/auth_controller.h"

#include "errors/errors.h"
#include "routes/auth_routes.h"
#include "util/logger.h"
#include "validators/user_validator.h"

struct AuthControllerV1 auth_controller_v1_new(void)
{
    struct AuthControllerV1 controller;
    controller.validator = user_validator_new();
    controller.routes = auth_routes_new("/api/v1/auth");
    return controller;
}

/// Entry point of the deep call chain. Validates, then hands off to the route.
const char *handle_login(struct AuthControllerV1 *controller, const char *email,
                         const char *password)
{
    struct Logger log = get_logger("api.v1.auth");
    logger_info(&log, "v1 login");

    enum AppError err = user_validator_validate(&controller->validator, email);
    if (err != APP_OK) {
        logger_warn(&log, app_error_name(err));
        return NULL;
    }

    return login_handler(&controller->routes, email, password);
}
