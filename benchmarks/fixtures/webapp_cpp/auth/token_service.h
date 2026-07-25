#pragma once

#include <string>

#include "auth/token_claims.h"
#include "database/database_connection.h"
#include "models/user.h"
#include "util/logger.h"

namespace webapp {
namespace auth {

/// Handles token generation, validation, and revocation.
class TokenService {
public:
    explicit TokenService(database::DatabaseConnection& db);

    /// Generate a new authentication token for a user and persist it.
    std::string generate_token(const models::User& user);

    /// Validate a token and return its claims.
    TokenClaims validate_token(const std::string& token);

    /// Refresh a token, returning a freshly generated one.
    std::string refresh_token(const std::string& old_token);

    /// Revoke a token so it can no longer be used.
    void revoke_token(const std::string& token);

    int token_expiry() const;

private:
    util::Logger log_;
    database::DatabaseConnection& db_;
    int token_expiry_;
};

}  // namespace auth
}  // namespace webapp
