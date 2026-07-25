#ifndef SERVICES_BASE_SERVICE_H
#define SERVICES_BASE_SERVICE_H

#include "errors/errors.h"

struct BaseService;

/// Virtual dispatch table for the service "class hierarchy".
///
/// C has no inheritance and no virtual methods, so overridable behaviour is
/// declared here as function pointers. Each derived service installs its own
/// implementations when it is constructed.
struct BaseServiceVTable {
    void (*initialize)(struct BaseService *self);
    void (*shutdown)(struct BaseService *self);
    const char *(*describe)(const struct BaseService *self);
};

/// Common state shared by every application service.
///
/// C HAS NO INHERITANCE. The classic C idiom for it is struct embedding: each
/// "derived" service (UserService, SessionService, AuthenticationService,
/// AdminService) declares `struct BaseService base;` as its FIRST member, so a
/// pointer to the derived struct can be cast to `struct BaseService *` and the
/// shared fields land at the same offsets. That upcast is what stands in for
/// `class UserService : BaseService` in an OO language.
struct BaseService {
    const char *service_name;
    int initialized;
    const struct BaseServiceVTable *vtable;
};

struct BaseService base_service_new(const char *service_name,
                                    const struct BaseServiceVTable *vtable);

/// Run the vtable's initialize hook, then mark the service ready.
void base_service_initialize(struct BaseService *self);

/// Guard used by every service entry point before it touches state.
enum AppError require_initialized(struct BaseService *self);
const char *base_service_name(const struct BaseService *self);

#endif /* SERVICES_BASE_SERVICE_H */
