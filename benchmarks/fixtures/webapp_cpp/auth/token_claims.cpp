#include "auth/token_claims.h"

#include <string>
#include <utility>

namespace webapp {
namespace auth {

TokenClaims::TokenClaims(std::string user_id, std::string email, std::string role)
    : user_id_(std::move(user_id)), email_(std::move(email)), role_(std::move(role)) {}

const std::string& TokenClaims::user_id() const {
    return user_id_;
}

const std::string& TokenClaims::email() const {
    return email_;
}

const std::string& TokenClaims::role() const {
    return role_;
}

}  // namespace auth
}  // namespace webapp
