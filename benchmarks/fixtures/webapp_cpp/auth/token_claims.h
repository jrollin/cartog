#pragma once

#include <string>

namespace webapp {
namespace auth {

/// Decoded claims carried by an authentication token.
class TokenClaims {
public:
    TokenClaims(std::string user_id, std::string email, std::string role);

    const std::string& user_id() const;
    const std::string& email() const;
    const std::string& role() const;

private:
    std::string user_id_;
    std::string email_;
    std::string role_;
};

}  // namespace auth
}  // namespace webapp
