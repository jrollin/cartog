#pragma once

#include <string>

namespace webapp {
namespace models {

/// Roles a user account can hold.
enum class UserRole { Guest, Member, Admin };

/// Application user record.
class User {
public:
    User(std::string id, std::string email, std::string password, std::string role);

    /// Validate the user's own fields, throwing ValidationError when invalid.
    void validate() const;

    bool check_password(const std::string& candidate) const;

    const std::string& id() const;
    const std::string& email() const;
    const std::string& role() const;

private:
    std::string id_;
    std::string email_;
    std::string password_;
    std::string role_;
};

}  // namespace models
}  // namespace webapp
