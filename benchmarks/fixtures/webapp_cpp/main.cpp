#include <exception>
#include <iostream>
#include <string>

#include "api/v1/auth_controller.h"
#include "api/v2/auth_controller.h"
#include "auth/auth_service.h"
#include "auth/token_service.h"
#include "database/database_connection.h"
#include "middleware/auth_middleware.h"
#include "models/payment.h"
#include "services/authentication_service.h"
#include "services/session_service.h"
#include "services/user_service.h"
#include "util/config.h"
#include "util/logger.h"
#include "validators/payment_validator.h"

/// Application entry point: wires the services and runs one login.
int main() {
    webapp::util::Logger main_log = webapp::util::Logger::get_logger("main");
    main_log.info("Starting webapp");

    webapp::util::Config config =
        webapp::util::Config::Builder().with_database_url("postgres://localhost/app").build();
    main_log.info("Config database url: " + config.database_url());

    webapp::database::DatabaseConnection db("localhost", 5432, "app");

    webapp::services::AuthenticationService auth_service(db);
    auth_service.initialize();

    webapp::services::UserService user_service(db);
    user_service.initialize();
    user_service.create_user("user@example.com");

    webapp::services::SessionService session_service(db);
    session_service.initialize();

    webapp::auth::TokenService token_service(db);
    webapp::auth::AuthService raw_auth(token_service);
    webapp::auth::AdminService admin_service(raw_auth);
    admin_service.initialize();

    webapp::middleware::AuthMiddleware middleware(token_service);

    webapp::api::v1::AuthController v1_controller;
    webapp::api::v2::AuthController v2_controller;

    try {
        std::string token = v1_controller.handle_login("user@example.com", "secret1");
        session_service.create(token);
        main_log.info("v1 token issued, admin=" +
                      std::string(admin_service.is_admin(token) ? "yes" : "no"));
        main_log.info("middleware ok=" + std::string(middleware.authenticate(token) ? "1" : "0"));

        std::string v2_token = v2_controller.handle_login("user@example.com", "secret1");
        main_log.info("v2 token issued: " + v2_token);

        webapp::models::Payment payment(19.99, "EUR");
        payment.validate();
        webapp::validators::PaymentValidator payment_validator;
        payment_validator.validate(payment.amount());
        payment_validator.validate_currency(payment.currency());
    } catch (const std::exception& err) {
        std::cerr << "request failed: " << err.what() << std::endl;
        return 1;
    }

    main_log.info("Shutting down webapp");
    return 0;
}
