#include "api/v1/auth_controller.h"

#include <string>

#include "util/logger.h"

namespace webapp {
namespace api {
namespace v1 {

AuthController::AuthController() : log_(util::Logger::get_logger("api.v1.auth")) {}

/// Entry point of the deep call chain: validate then delegate to the route.
std::string AuthController::handle_login(const std::string& email, const std::string& password) {
    log_.info("v1 login");
    validator_.validate(email);
    return routes_.login_handler(email, password);
}

}  // namespace v1
}  // namespace api
}  // namespace webapp
