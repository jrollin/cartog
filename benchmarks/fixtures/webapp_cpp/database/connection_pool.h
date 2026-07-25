#pragma once

#include "util/logger.h"

namespace webapp {
namespace database {

/// A single pooled connection handle.
class ConnectionHandle {
public:
    explicit ConnectionHandle(int id);

    int id() const;
    bool in_use() const;
    void mark_used();
    void mark_free();

private:
    int id_;
    bool in_use_;
};

/// Pool of reusable database connection handles.
class ConnectionPool {
public:
    explicit ConnectionPool(int size);

    /// Acquire a handle from the pool. Logged leaf of the deep call chain.
    ConnectionHandle get_connection();

    /// Return a handle to the pool so it can be reused.
    void release_connection(ConnectionHandle& handle);

    int size() const;

private:
    util::Logger log_;
    int size_;
    int next_id_;
};

}  // namespace database
}  // namespace webapp
