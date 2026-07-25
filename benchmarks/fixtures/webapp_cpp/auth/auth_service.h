#pragma once

#include <string>

#include "auth/auth_provider.h"
#include "auth/token_service.h"
#include "models/user.h"
#include "services/base_service.h"
#include "util/logger.h"

namespace webapp {
namespace auth {

/// Handles user authentication flows on top of a TokenService.
class AuthService : public AuthProvider {
public:
    explicit AuthService(TokenService& token_service);

    /// Verify credentials and mint a token. Hop 3 of the deep call chain.
    std::string login(const std::string& email, const std::string& password) override;

    /// Revoke the caller's token.
    void logout(const std::string& token) override;

    /// Resolve the user behind a token.
    models::User get_current_user(const std::string& token);

private:
    util::Logger log_;
    TokenService& token_service_;
};

/// Authentication with admin-role checks layered on top.
class AdminService : public services::BaseService {
public:
    explicit AdminService(AuthService& auth_service);

    /// True when the token's user carries the admin role.
    bool is_admin(const std::string& token);

private:
    AuthService& auth_service_;
};

}  // namespace auth
}  // namespace webapp
