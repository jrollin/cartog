#pragma once

#include <string>

#include "routes/auth_routes.h"
#include "util/logger.h"
#include "validators/user_validator.h"

namespace webapp {
namespace api {
namespace v1 {

/// v1 authentication HTTP controller.
class AuthController {
public:
    AuthController();

    /// Entry point of the deep call chain: validate then delegate to the route.
    std::string handle_login(const std::string& email, const std::string& password);

private:
    util::Logger log_;
    validators::UserValidator validator_;
    routes::LoginHandler routes_;
};

}  // namespace v1
}  // namespace api
}  // namespace webapp
