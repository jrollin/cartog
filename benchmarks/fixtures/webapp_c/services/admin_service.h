#ifndef SERVICES_ADMIN_SERVICE_H
#define SERVICES_ADMIN_SERVICE_H

#include "errors/errors.h"
#include "services/authentication_service.h"
#include "services/base_service.h"

/// Admin-only operations gated on the caller's role.
/// `base` first = the C struct-embedding stand-in for inheritance.
struct AdminService {
    struct BaseService base;
    struct AuthenticationService *auth;
};

struct AdminService admin_service_new(struct AuthenticationService *auth);
enum AppError admin_service_promote(struct AdminService *service, const char *token,
                                   const char *email);

#endif /* SERVICES_ADMIN_SERVICE_H */
