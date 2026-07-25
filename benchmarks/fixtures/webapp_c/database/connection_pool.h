#ifndef DATABASE_CONNECTION_POOL_H
#define DATABASE_CONNECTION_POOL_H

/// A single pooled connection handle.
struct ConnectionHandle {
    int id;
    int in_use;
};

/// Pool of reusable connection handles.
struct ConnectionPool {
    int size;
    int next_id;
};

struct ConnectionPool connection_pool_new(int size);

/// Acquire a handle from the pool. Leaf of the deep call chain: it logs and
/// returns without calling further into the app.
struct ConnectionHandle get_connection(struct ConnectionPool *pool);
void release_connection(struct ConnectionPool *pool, struct ConnectionHandle *handle);

#endif /* DATABASE_CONNECTION_POOL_H */
