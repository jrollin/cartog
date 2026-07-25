#pragma once

#include <string>

#include "util/logger.h"

namespace webapp {
namespace services {

/// Common base for all application services.
class BaseService {
public:
    explicit BaseService(std::string service_name);
    virtual ~BaseService() = default;

    /// Mark the service ready to serve traffic.
    void initialize();

    const std::string& name() const;
    bool initialized() const;

protected:
    /// Warn when a caller reached the service before initialize().
    void require_initialized() const;

private:
    util::Logger log_;
    std::string service_name_;
    bool initialized_;
};

}  // namespace services
}  // namespace webapp
