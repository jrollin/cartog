#ifndef DATABASE_DATABASE_CONNECTION_H
#define DATABASE_DATABASE_CONNECTION_H

#include "database/connection_pool.h"

/// A single database connection with query execution support.
struct DatabaseConnection {
    const char *host;
    int port;
    const char *database;
    struct ConnectionPool pool;
};

struct DatabaseConnection database_connection_new(const char *host, int port, const char *database);

/// Execute a query, borrowing a pooled connection for its duration.
int execute_query(struct DatabaseConnection *db, const char *query);
const char *insert(struct DatabaseConnection *db, const char *table, const char *values);

#endif /* DATABASE_DATABASE_CONNECTION_H */
