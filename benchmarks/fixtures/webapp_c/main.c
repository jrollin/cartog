#include <stdio.h>
#include <stddef.h>

#include "api/v1/auth_controller.h"
#include "api/v2/auth_controller.h"
#include "auth/token_service.h"
#include "database/database_connection.h"
#include "middleware/auth_middleware.h"
#include "models/payment.h"
#include "models/user.h"
#include "services/admin_service.h"
#include "services/authentication_service.h"
#include "services/session_service.h"
#include "services/user_service.h"
#include "util/config.h"
#include "util/logger.h"
#include "validators/payment_validator.h"

/// Wires the service graph and exercises one login through each API version.
int main(void)
{
    struct Logger log = get_logger("main");
    logger_info(&log, "Starting webapp");

    struct Config config = default_config();
    printf("[main] database: %s port: %d\n", config.database_url, config.port);

    struct DatabaseConnection db = database_connection_new("localhost", 5432, "app");

    struct UserService users = user_service_new(&db);
    base_service_initialize(&users.base);
    user_service_create_user(&users, "user@example.com");

    struct SessionService sessions = session_service_new(&db);
    base_service_initialize(&sessions.base);

    struct AuthenticationService auth = authentication_service_new(&db);
    base_service_initialize(&auth.base);

    struct AdminService admin = admin_service_new(&auth);
    base_service_initialize(&admin.base);

    struct AuthControllerV1 v1 = auth_controller_v1_new();
    const char *v1_token = handle_login(&v1, "user@example.com", "secret1");
    printf("[main] v1 token: %s\n", v1_token == NULL ? "(none)" : v1_token);

    struct AuthControllerV2 v2 = auth_controller_v2_new(1);
    const char *v2_token = auth_controller_v2_login(&v2, "user@example.com", "secret1");
    printf("[main] v2 token: %s\n", v2_token == NULL ? "(none)" : v2_token);

    struct TokenService tokens = token_service_new(&db);
    struct AuthMiddleware middleware = auth_middleware_new(&tokens);
    auth_middleware_handle(&middleware, "jwt_user_1_token");
    printf("[main] refreshed: %s\n", refresh_token(&tokens, "jwt_user_1_token"));

    struct User user = user_new("user_1", "user@example.com", "secret1", "admin");
    user_validate(&user);
    admin_service_promote(&admin, "jwt_user_1_token", "user@example.com");

    struct Payment payment = payment_new("pay_1", 42.5, "EUR");
    struct PaymentValidator payments = payment_validator_new(1000.0);
    payment_validator_validate(&payments, &payment);
    session_service_create(&sessions, "jwt_user_1_token");

    logger_info(&log, "Shutting down");
    return 0;
}
