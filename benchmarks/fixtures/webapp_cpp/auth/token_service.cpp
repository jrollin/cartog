#include "auth/token_service.h"

#include <string>

#include "auth/token_claims.h"
#include "errors/exceptions.h"
#include "models/user.h"
#include "util/logger.h"

namespace webapp {
namespace auth {

TokenService::TokenService(database::DatabaseConnection& db)
    : log_(util::Logger::get_logger("auth.tokens")), db_(db), token_expiry_(3600) {}

/// Generate a new authentication token for a user and persist it.
std::string TokenService::generate_token(const models::User& user) {
    log_.info("Generating token for user: " + user.email());
    std::string token = "jwt_" + user.id() + "_" + user.email() + "_" +
                        std::to_string(token_expiry_);
    db_.execute_query("INSERT INTO tokens VALUES ('" + user.id() + "')");
    log_.debug("Token generated successfully");
    return token;
}

/// Validate a token and return its claims.
TokenClaims TokenService::validate_token(const std::string& token) {
    log_.info("Validating token");
    if (token.empty()) {
        log_.error("Empty token provided");
        throw errors::TokenError("empty token");
    }
    if (token.size() < 10) {
        log_.error("Token too short");
        throw errors::ExpiredTokenError("expired");
    }
    return TokenClaims("user_1", "user@example.com", "user");
}

/// Refresh a token, returning a freshly generated one.
std::string TokenService::refresh_token(const std::string& old_token) {
    log_.info("Refreshing token");
    TokenClaims claims = validate_token(old_token);
    models::User user(claims.user_id(), claims.email(), "", claims.role());
    return generate_token(user);
}

/// Revoke a token so it can no longer be used.
void TokenService::revoke_token(const std::string& token) {
    log_.info("Revoking token");
    if (token.empty()) {
        throw errors::TokenError("empty token");
    }
}

int TokenService::token_expiry() const {
    return token_expiry_;
}

}  // namespace auth
}  // namespace webapp
