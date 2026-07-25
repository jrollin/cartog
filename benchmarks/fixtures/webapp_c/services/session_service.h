#ifndef SERVICES_SESSION_SERVICE_H
#define SERVICES_SESSION_SERVICE_H

#include "database/database_connection.h"
#include "errors/errors.h"
#include "services/base_service.h"

/// Manages user sessions backed by the database.
/// `base` first = the C struct-embedding stand-in for inheritance.
struct SessionService {
    struct BaseService base;
    struct DatabaseConnection *db;
};

struct SessionService session_service_new(struct DatabaseConnection *db);
enum AppError session_service_create(struct SessionService *service, const char *token);
enum AppError session_service_destroy(struct SessionService *service, const char *token);

#endif /* SERVICES_SESSION_SERVICE_H */
