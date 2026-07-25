#pragma once

#include <string>

#include "util/logger.h"

namespace webapp {
namespace routes {

/// HTTP route handlers for the authentication endpoints.
class LoginHandler {
public:
    LoginHandler();

    /// POST /login — hop 1.5 of the deep call chain.
    std::string login_handler(const std::string& email, const std::string& password);

    /// POST /logout — revoke the caller's session.
    std::string logout_handler(const std::string& token);

private:
    util::Logger log_;
};

}  // namespace routes
}  // namespace webapp
