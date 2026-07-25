#ifndef SERVICES_USER_SERVICE_H
#define SERVICES_USER_SERVICE_H

#include "database/database_connection.h"
#include "errors/errors.h"
#include "services/base_service.h"

/// User CRUD backed by the database.
///
/// `base` is FIRST so `(struct BaseService *)&user_service` is a valid upcast:
/// the C stand-in for `class UserService : BaseService`.
struct UserService {
    struct BaseService base;
    struct DatabaseConnection *db;
};

struct UserService user_service_new(struct DatabaseConnection *db);
enum AppError user_service_create_user(struct UserService *service, const char *email);

#endif /* SERVICES_USER_SERVICE_H */
