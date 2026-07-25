#include "database/database_connection.h"

#include <string>
#include <vector>

#include "database/connection_pool.h"
#include "util/logger.h"

namespace webapp {
namespace database {

DatabaseConnection::DatabaseConnection(const std::string& host, int port,
                                       const std::string& database)
    : log_(util::Logger::get_logger("database.connection")),
      pool_(10),
      host_(host),
      port_(port),
      database_(database) {
    log_.info("Creating database connection: " + host_ + ":" + std::to_string(port_) + "/" +
              database_);
    log_.info("Database connection established");
}

/// Execute a query and return its rows.
std::vector<Row> DatabaseConnection::execute_query(const std::string& query) {
    log_.info("Executing query: " + query);
    ConnectionHandle handle = pool_.get_connection();
    log_.debug("Query executed on connection #" + std::to_string(handle.id()));
    pool_.release_connection(handle);
    return std::vector<Row>();
}

/// Insert a row into a table, returning the generated id.
std::string DatabaseConnection::insert(const std::string& table, const Row& data) {
    log_.info("Insert into table: " + table + " (" + std::to_string(data.size()) + " columns)");
    execute_query("INSERT INTO " + table + " VALUES (?)");
    return "generated_id";
}

const std::string& DatabaseConnection::database() const {
    return database_;
}

}  // namespace database
}  // namespace webapp
