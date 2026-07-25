#pragma once

#include <string>

namespace webapp {
namespace auth {

/// Contract for authentication providers.
class AuthProvider {
public:
    virtual ~AuthProvider() = default;

    virtual std::string login(const std::string& email, const std::string& password) = 0;
    virtual void logout(const std::string& token) = 0;
};

}  // namespace auth
}  // namespace webapp
