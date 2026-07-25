#include "auth/auth_service.h"

#include <string>

#include "auth/token_claims.h"
#include "errors/exceptions.h"
#include "models/user.h"
#include "util/logger.h"

namespace webapp {
namespace auth {

AuthService::AuthService(TokenService& token_service)
    : log_(util::Logger::get_logger("auth.service")), token_service_(token_service) {}

/// Verify credentials and mint a token. Hop 3 of the deep call chain.
std::string AuthService::login(const std::string& email, const std::string& password) {
    log_.info("Login attempt for: " + email);
    if (email.empty()) {
        log_.warn("Empty email on login");
        throw errors::AuthenticationError("email is required");
    }
    if (password.size() < 6) {
        log_.warn("Invalid password for: " + email);
        throw errors::AuthenticationError("invalid credentials");
    }
    models::User user("user_1", email, password, "user");
    user.validate();
    std::string token = token_service_.generate_token(user);
    log_.info("Login successful for: " + email);
    return token;
}

/// Revoke the caller's token.
void AuthService::logout(const std::string& token) {
    log_.info("Logout request");
    token_service_.revoke_token(token);
}

/// Resolve the user behind a token.
models::User AuthService::get_current_user(const std::string& token) {
    log_.info("Getting current user from token");
    TokenClaims claims = token_service_.validate_token(token);
    return models::User(claims.user_id(), claims.email(), "", claims.role());
}

AdminService::AdminService(AuthService& auth_service)
    : services::BaseService("admin"), auth_service_(auth_service) {}

/// True when the token's user carries the admin role.
bool AdminService::is_admin(const std::string& token) {
    models::User user = auth_service_.get_current_user(token);
    return user.role() == "admin";
}

}  // namespace auth
}  // namespace webapp
