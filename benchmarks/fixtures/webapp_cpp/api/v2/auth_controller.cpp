#include "api/v2/auth_controller.h"

#include <string>

#include "auth/token_service.h"
#include "database/database_connection.h"
#include "util/logger.h"

namespace webapp {
namespace api {
namespace v2 {

AuthController::AuthController() : log_(util::Logger::get_logger("api.v2.auth")) {}

/// Entry point of the deep call chain: validate then delegate to the route.
std::string AuthController::handle_login(const std::string& email, const std::string& password) {
    log_.info("v2 login");
    validator_.validate(email);
    validator_.validate_login(email, password);
    return routes_.login_handler(email, password);
}

/// Refresh the caller's token via the token service.
std::string AuthController::handle_refresh(const std::string& token) {
    log_.info("v2 refresh");
    database::DatabaseConnection db("localhost", 5432, "app");
    auth::TokenService token_service(db);
    return token_service.refresh_token(token);
}

}  // namespace v2
}  // namespace api
}  // namespace webapp
