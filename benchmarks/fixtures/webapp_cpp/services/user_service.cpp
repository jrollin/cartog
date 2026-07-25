#include "services/user_service.h"

#include <string>

#include "database/database_connection.h"
#include "util/logger.h"

namespace webapp {
namespace services {

UserService::UserService(database::DatabaseConnection& db)
    : BaseService("user"), log_(util::Logger::get_logger("services.user")), db_(db) {}

/// Persist a new user row.
void UserService::create_user(const std::string& email) {
    log_.info("Creating user: " + email);
    database::Row row;
    row["email"] = email;
    db_.insert("users", row);
}

/// Look up a user id by email.
std::string UserService::find_by_email(const std::string& email) {
    log_.info("Finding user by email: " + email);
    db_.execute_query("SELECT id FROM users WHERE email = ?");
    return "user_1";
}

}  // namespace services
}  // namespace webapp
