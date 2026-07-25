#include "models/user.h"

#include <string>
#include <utility>

#include "errors/exceptions.h"

namespace webapp {
namespace models {

User::User(std::string id, std::string email, std::string password, std::string role)
    : id_(std::move(id)),
      email_(std::move(email)),
      password_(std::move(password)),
      role_(std::move(role)) {}

/// Validate the user's own fields, throwing ValidationError when invalid.
void User::validate() const {
    if (email_.empty()) {
        throw errors::ValidationError("email is required");
    }
    if (role_.empty()) {
        throw errors::ValidationError("role is required");
    }
}

bool User::check_password(const std::string& candidate) const {
    return !candidate.empty() && candidate == password_;
}

const std::string& User::id() const {
    return id_;
}

const std::string& User::email() const {
    return email_;
}

const std::string& User::role() const {
    return role_;
}

}  // namespace models
}  // namespace webapp
