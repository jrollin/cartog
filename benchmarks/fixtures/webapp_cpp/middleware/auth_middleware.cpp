#include "middleware/auth_middleware.h"

#include <string>

#include "auth/token_claims.h"
#include "util/logger.h"

namespace webapp {
namespace middleware {

AuthMiddleware::AuthMiddleware(auth::TokenService& token_service)
    : log_(util::Logger::get_logger("middleware.auth")), token_service_(token_service) {}

/// Validate the request's bearer token.
bool AuthMiddleware::authenticate(const std::string& token) {
    log_.info("Authenticating request");
    auth::TokenClaims claims = token_service_.validate_token(token);
    return !claims.user_id().empty();
}

/// Extract the bearer token from an Authorization header value.
std::string AuthMiddleware::extract_token(const std::string& header) const {
    const std::string prefix = "Bearer ";
    if (header.rfind(prefix, 0) != 0) {
        log_.warn("Authorization header is not a bearer token");
        return std::string();
    }
    return header.substr(prefix.size());
}

}  // namespace middleware
}  // namespace webapp
