#pragma once

#include <string>

#include "database/database_connection.h"
#include "services/base_service.h"
#include "util/logger.h"

namespace webapp {
namespace services {

/// Manages user sessions backed by the database.
class SessionService : public BaseService {
public:
    explicit SessionService(database::DatabaseConnection& db);

    /// Persist a new session row for a token.
    void create(const std::string& token);

    /// Drop the session row for a token.
    void destroy(const std::string& token);

private:
    util::Logger log_;
    database::DatabaseConnection& db_;
};

}  // namespace services
}  // namespace webapp
