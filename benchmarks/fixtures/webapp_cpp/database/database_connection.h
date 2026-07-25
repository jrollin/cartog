#pragma once

#include <map>
#include <string>
#include <vector>

#include "database/connection_pool.h"
#include "util/logger.h"

namespace webapp {
namespace database {

/// One row of a query result, keyed by column name.
using Row = std::map<std::string, std::string>;

/// A single database connection with query execution support.
class DatabaseConnection {
public:
    DatabaseConnection(const std::string& host, int port, const std::string& database);

    /// Execute a query and return its rows.
    std::vector<Row> execute_query(const std::string& query);

    /// Insert a row into a table, returning the generated id.
    std::string insert(const std::string& table, const Row& data);

    const std::string& database() const;

private:
    util::Logger log_;
    ConnectionPool pool_;
    std::string host_;
    int port_;
    std::string database_;
};

/// Generic repository over a table.
template <typename T>
class Repository {
public:
    explicit Repository(DatabaseConnection& db) : db_(db) {}

    void save(const T& entity) {
        (void)entity;
        db_.execute_query("INSERT");
    }

private:
    DatabaseConnection& db_;
};

}  // namespace database
}  // namespace webapp
