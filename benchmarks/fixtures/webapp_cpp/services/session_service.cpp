#include "services/session_service.h"

#include <string>

#include "database/database_connection.h"
#include "util/logger.h"

namespace webapp {
namespace services {

SessionService::SessionService(database::DatabaseConnection& db)
    : BaseService("session"), log_(util::Logger::get_logger("services.session")), db_(db) {}

/// Persist a new session row for a token.
void SessionService::create(const std::string& token) {
    log_.info("Creating session");
    database::Row row;
    row["token"] = token;
    db_.insert("sessions", row);
}

/// Drop the session row for a token.
void SessionService::destroy(const std::string& token) {
    log_.info("Destroying session");
    db_.execute_query("DELETE FROM sessions WHERE token = '" + token + "'");
}

}  // namespace services
}  // namespace webapp
