#ifndef API_V2_AUTH_CONTROLLER_H
#define API_V2_AUTH_CONTROLLER_H

#include "routes/auth_routes.h"
#include "validators/user_validator.h"

/// v2 authentication HTTP controller.
struct AuthControllerV2 {
    struct UserValidator validator;
    struct AuthRoutes routes;
    int require_mfa;
};

struct AuthControllerV2 auth_controller_v2_new(int require_mfa);

/// Public wrapper over the file-local v2 `handle_login`. C has no namespaces,
/// so only one `handle_login` can have external linkage (v1's); v2 keeps the
/// same handler name file-local and exports this differently-named entry.
const char *auth_controller_v2_login(struct AuthControllerV2 *controller, const char *email,
                                     const char *password);

#endif /* API_V2_AUTH_CONTROLLER_H */
