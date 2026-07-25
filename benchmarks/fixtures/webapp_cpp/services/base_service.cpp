#include "services/base_service.h"

#include <string>
#include <utility>

#include "util/logger.h"

namespace webapp {
namespace services {

BaseService::BaseService(std::string service_name)
    : log_(util::Logger::get_logger("services.base")),
      service_name_(std::move(service_name)),
      initialized_(false) {}

/// Mark the service ready to serve traffic.
void BaseService::initialize() {
    log_.info("Initializing service: " + service_name_);
    initialized_ = true;
}

const std::string& BaseService::name() const {
    return service_name_;
}

bool BaseService::initialized() const {
    return initialized_;
}

/// Warn when a caller reached the service before initialize().
void BaseService::require_initialized() const {
    if (!initialized_) {
        log_.warn(service_name_ + " is not initialized");
    }
}

}  // namespace services
}  // namespace webapp
