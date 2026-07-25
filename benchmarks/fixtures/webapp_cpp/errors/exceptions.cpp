#include "errors/exceptions.h"

#include <string>

namespace webapp {
namespace errors {

AppException::AppException(const std::string& message) : std::runtime_error(message) {}

ValidationError::ValidationError(const std::string& message) : AppException(message) {}

AuthenticationError::AuthenticationError(const std::string& message) : AppException(message) {}

TokenError::TokenError(const std::string& message) : AppException(message) {}

ExpiredTokenError::ExpiredTokenError(const std::string& message) : TokenError(message) {}

}  // namespace errors
}  // namespace webapp
