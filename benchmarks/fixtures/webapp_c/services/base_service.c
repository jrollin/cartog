#include <stddef.h>

#include "services/base_service.h"

#include "errors/errors.h"
#include "util/logger.h"

struct BaseService base_service_new(const char *service_name,
                                    const struct BaseServiceVTable *vtable)
{
    struct BaseService self;
    self.service_name = service_name;
    self.initialized = 0;
    self.vtable = vtable;
    return self;
}

/// Dispatches through the vtable, so a derived service's own hook runs first.
void base_service_initialize(struct BaseService *self)
{
    struct Logger log = get_logger("services.base");
    logger_info(&log, self->service_name);

    if (self->vtable != NULL && self->vtable->initialize != NULL) {
        self->vtable->initialize(self);
    }
    self->initialized = 1;
}

enum AppError require_initialized(struct BaseService *self)
{
    if (!self->initialized) {
        struct Logger log = get_logger("services.base");
        logger_warn(&log, "Service is not initialized");
        return APP_ERROR_GENERIC;
    }
    return APP_OK;
}

const char *base_service_name(const struct BaseService *self)
{
    if (self->vtable != NULL && self->vtable->describe != NULL) {
        return self->vtable->describe(self);
    }
    return self->service_name;
}
