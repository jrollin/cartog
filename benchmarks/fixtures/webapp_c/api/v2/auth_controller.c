#include <stddef.h>

#include "api/v2/auth_controller.h"

#include "errors/errors.h"
#include "routes/auth_routes.h"
#include "util/logger.h"
#include "validators/user_validator.h"

/// File-local twin of the v1 handler: same name, internal linkage.
static const char *handle_login(struct AuthControllerV2 *controller, const char *email,
                                const char *password)
{
    struct Logger log = get_logger("api.v2.auth");
    logger_info(&log, "v2 login");

    enum AppError err = user_validator_validate_login(&controller->validator, email, password);
    if (err != APP_OK) {
        logger_warn(&log, app_error_name(err));
        return NULL;
    }
    if (controller->require_mfa) {
        logger_debug(&log, "MFA challenge required");
    }

    return login_handler(&controller->routes, email, password);
}

struct AuthControllerV2 auth_controller_v2_new(int require_mfa)
{
    struct AuthControllerV2 controller;
    controller.validator = user_validator_new();
    controller.routes = auth_routes_new("/api/v2/auth");
    controller.require_mfa = require_mfa;
    return controller;
}

const char *auth_controller_v2_login(struct AuthControllerV2 *controller, const char *email,
                                     const char *password)
{
    return handle_login(controller, email, password);
}
