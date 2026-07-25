#pragma once

#include <string>

#include "util/logger.h"

namespace webapp {
namespace validators {

/// Validates user-facing input before it reaches a service.
class UserValidator {
public:
    UserValidator();

    /// Reject an empty email address.
    void validate(const std::string& email) const;

    /// Reject an empty email or a too-short password.
    void validate_login(const std::string& email, const std::string& password) const;

private:
    util::Logger log_;
};

}  // namespace validators
}  // namespace webapp
