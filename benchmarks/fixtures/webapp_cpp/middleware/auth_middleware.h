#pragma once

#include <string>

#include "auth/token_service.h"
#include "util/logger.h"

namespace webapp {
namespace middleware {

/// Middleware that authenticates a request by validating its token.
class AuthMiddleware {
public:
    explicit AuthMiddleware(auth::TokenService& token_service);

    /// Validate the request's bearer token.
    bool authenticate(const std::string& token);

    /// Extract the bearer token from an Authorization header value.
    std::string extract_token(const std::string& header) const;

private:
    util::Logger log_;
    auth::TokenService& token_service_;
};

}  // namespace middleware
}  // namespace webapp
