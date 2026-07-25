#ifndef API_V1_AUTH_CONTROLLER_H
#define API_V1_AUTH_CONTROLLER_H

#include "routes/auth_routes.h"
#include "validators/user_validator.h"

/// v1 authentication HTTP controller.
struct AuthControllerV1 {
    struct UserValidator validator;
    struct AuthRoutes routes;
};

struct AuthControllerV1 auth_controller_v1_new(void);

/// Entry point of the deep call chain:
/// handle_login -> authenticate -> login -> generate_token -> execute_query
///   -> get_connection
const char *handle_login(struct AuthControllerV1 *controller, const char *email,
                         const char *password);

#endif /* API_V1_AUTH_CONTROLLER_H */
