#include "validators/user_validator.h"

#include <string>

#include "errors/exceptions.h"
#include "util/logger.h"

namespace webapp {
namespace validators {

UserValidator::UserValidator() : log_(util::Logger::get_logger("validators.user")) {}

/// Reject an empty email address.
void UserValidator::validate(const std::string& email) const {
    log_.info("Validating user input");
    if (email.empty()) {
        throw errors::ValidationError("email is required");
    }
}

/// Reject an empty email or a too-short password.
void UserValidator::validate_login(const std::string& email, const std::string& password) const {
    validate(email);
    if (password.size() < 6) {
        throw errors::ValidationError("password too short");
    }
}

}  // namespace validators
}  // namespace webapp
