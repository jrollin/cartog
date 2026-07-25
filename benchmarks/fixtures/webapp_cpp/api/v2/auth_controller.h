#pragma once

#include <string>

#include "middleware/auth_middleware.h"
#include "routes/auth_routes.h"
#include "util/logger.h"
#include "validators/user_validator.h"

namespace webapp {
namespace api {
namespace v2 {

/// v2 authentication HTTP controller, with stricter login validation.
class AuthController {
public:
    AuthController();

    /// Entry point of the deep call chain: validate then delegate to the route.
    std::string handle_login(const std::string& email, const std::string& password);

    /// Refresh the caller's token via the token service.
    std::string handle_refresh(const std::string& token);

private:
    util::Logger log_;
    validators::UserValidator validator_;
    routes::LoginHandler routes_;
};

}  // namespace v2
}  // namespace api
}  // namespace webapp
