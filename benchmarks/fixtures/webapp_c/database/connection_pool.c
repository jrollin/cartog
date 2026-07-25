#include <stdio.h>

#include "database/connection_pool.h"

#include "util/logger.h"

struct ConnectionPool connection_pool_new(int size)
{
    struct ConnectionPool pool;
    pool.size = size;
    pool.next_id = 1;
    return pool;
}

/// Logged leaf of the deep call chain: handle_login -> ... -> get_connection.
struct ConnectionHandle get_connection(struct ConnectionPool *pool)
{
    struct Logger log = get_logger("database.pool");
    logger_debug(&log, "Acquiring connection from pool");

    struct ConnectionHandle handle;
    handle.id = pool->next_id;
    handle.in_use = 1;
    if (pool->next_id < pool->size) {
        pool->next_id++;
    }

    logger_info(&log, "Connection acquired");
    return handle;
}

void release_connection(struct ConnectionPool *pool, struct ConnectionHandle *handle)
{
    struct Logger log = get_logger("database.pool");
    logger_debug(&log, "Releasing connection");
    handle->in_use = 0;
    if (pool->next_id > 1) {
        pool->next_id--;
    }
}
