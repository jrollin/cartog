#include "database/connection_pool.h"

#include <string>

#include "util/logger.h"

namespace webapp {
namespace database {

ConnectionHandle::ConnectionHandle(int id) : id_(id), in_use_(false) {}

int ConnectionHandle::id() const {
    return id_;
}

bool ConnectionHandle::in_use() const {
    return in_use_;
}

void ConnectionHandle::mark_used() {
    in_use_ = true;
}

void ConnectionHandle::mark_free() {
    in_use_ = false;
}

ConnectionPool::ConnectionPool(int size)
    : log_(util::Logger::get_logger("database.pool")), size_(size), next_id_(1) {}

/// Acquire a handle from the pool. Logged leaf of the deep call chain.
ConnectionHandle ConnectionPool::get_connection() {
    log_.debug("Acquiring connection from pool");
    ConnectionHandle handle(next_id_);
    handle.mark_used();
    log_.info("Connection acquired");
    return handle;
}

/// Return a handle to the pool so it can be reused.
void ConnectionPool::release_connection(ConnectionHandle& handle) {
    log_.debug("Releasing connection");
    handle.mark_free();
}

int ConnectionPool::size() const {
    return size_;
}

}  // namespace database
}  // namespace webapp
