#pragma once

#include <string>

#include "database/database_connection.h"
#include "services/base_service.h"
#include "util/logger.h"

namespace webapp {
namespace services {

/// User CRUD operations backed by the database.
class UserService : public BaseService {
public:
    explicit UserService(database::DatabaseConnection& db);

    /// Persist a new user row.
    void create_user(const std::string& email);

    /// Look up a user id by email.
    std::string find_by_email(const std::string& email);

private:
    util::Logger log_;
    database::DatabaseConnection& db_;
};

}  // namespace services
}  // namespace webapp
