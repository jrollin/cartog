#include "routes/auth_routes.h"

#include <string>

#include "database/database_connection.h"
#include "services/authentication_service.h"
#include "util/logger.h"

namespace webapp {
namespace routes {

LoginHandler::LoginHandler() : log_(util::Logger::get_logger("routes.auth")) {}

/// POST /login — hop 1.5 of the deep call chain.
std::string LoginHandler::login_handler(const std::string& email, const std::string& password) {
    log_.info("Handling login request");
    database::DatabaseConnection db("localhost", 5432, "app");
    services::AuthenticationService auth_svc(db);
    auth_svc.initialize();
    return auth_svc.authenticate(email, password);
}

/// POST /logout — revoke the caller's session.
std::string LoginHandler::logout_handler(const std::string& token) {
    log_.info("Handling logout request");
    database::DatabaseConnection db("localhost", 5432, "app");
    services::AuthenticationService auth_svc(db);
    auth_svc.initialize();
    auth_svc.logout(token);
    return "ok";
}

}  // namespace routes
}  // namespace webapp
