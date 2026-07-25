#include "services/authentication_service.h"

#include <string>

#include "database/database_connection.h"
#include "models/user.h"
#include "util/logger.h"

namespace webapp {
namespace services {

AuthenticationService::AuthenticationService(database::DatabaseConnection& db)
    : BaseService("authentication"),
      log_(util::Logger::get_logger("services.authentication")),
      db_(db),
      token_service_(db),
      auth_service_(token_service_) {
    log_.info("Creating AuthenticationService");
}

/// Perform the full authentication flow. Hop 2 of the deep call chain.
std::string AuthenticationService::authenticate(const std::string& email,
                                               const std::string& password) {
    require_initialized();
    log_.info("Authenticating user: " + email);

    std::string token = auth_service_.login(email, password);

    database::Row session;
    session["token"] = token;
    session["email"] = email;
    db_.insert("sessions", session);

    log_.info("Authentication successful for: " + email);
    return token;
}

/// Tear down the caller's session.
void AuthenticationService::logout(const std::string& token) {
    log_.info("Logging out");
    auth_service_.logout(token);
}

/// Resolve the user behind a token.
models::User AuthenticationService::get_current_user(const std::string& token) {
    log_.info("Getting current user");
    return auth_service_.get_current_user(token);
}

}  // namespace services
}  // namespace webapp
