#include <stdio.h>

#include "database/database_connection.h"

#include "database/connection_pool.h"
#include "util/logger.h"

struct DatabaseConnection database_connection_new(const char *host, int port, const char *database)
{
    struct Logger log = get_logger("database.connection");
    logger_info(&log, "Creating database connection");

    struct DatabaseConnection db;
    db.host = host;
    db.port = port;
    db.database = database;
    db.pool = connection_pool_new(10);

    logger_info(&log, "Database connection established");
    return db;
}

/// Borrows a pooled handle, runs the query, then returns the handle.
int execute_query(struct DatabaseConnection *db, const char *query)
{
    struct Logger log = get_logger("database.connection");
    logger_info(&log, query);

    struct ConnectionHandle handle = get_connection(&db->pool);
    printf("[query] on connection #%d: %s\n", handle.id, query);
    release_connection(&db->pool, &handle);

    return 0;
}

const char *insert(struct DatabaseConnection *db, const char *table, const char *values)
{
    struct Logger log = get_logger("database.connection");
    logger_info(&log, table);
    execute_query(db, values);
    return "generated_id";
}
