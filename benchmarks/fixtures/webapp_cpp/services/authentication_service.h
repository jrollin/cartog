#pragma once

#include <string>

#include "auth/auth_service.h"
#include "auth/token_service.h"
#include "database/database_connection.h"
#include "models/user.h"
#include "services/base_service.h"
#include "util/logger.h"

namespace webapp {
namespace services {

/// Orchestrates the full authentication workflow.
///
/// Deep call chain entry point:
/// authenticate() -> login() -> generate_token() -> execute_query() -> get_connection()
class AuthenticationService : public BaseService {
public:
    explicit AuthenticationService(database::DatabaseConnection& db);

    /// Perform the full authentication flow. Hop 2 of the deep call chain.
    std::string authenticate(const std::string& email, const std::string& password);

    /// Tear down the caller's session.
    void logout(const std::string& token);

    /// Resolve the user behind a token.
    models::User get_current_user(const std::string& token);

private:
    util::Logger log_;
    database::DatabaseConnection& db_;
    auth::TokenService token_service_;
    auth::AuthService auth_service_;
};

}  // namespace services
}  // namespace webapp
