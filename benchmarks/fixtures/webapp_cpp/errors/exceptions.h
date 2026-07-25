#pragma once

#include <stdexcept>
#include <string>

namespace webapp {
namespace errors {

/// Base class for all application exceptions.
class AppException : public std::runtime_error {
public:
    explicit AppException(const std::string& message);
};

/// Raised when user-supplied input fails validation.
class ValidationError : public AppException {
public:
    explicit ValidationError(const std::string& message);
};

/// Raised when credentials are missing or wrong.
class AuthenticationError : public AppException {
public:
    explicit AuthenticationError(const std::string& message);
};

/// Raised for any token-lifecycle failure.
class TokenError : public AppException {
public:
    explicit TokenError(const std::string& message);
};

/// Raised when a token is past its expiry window.
class ExpiredTokenError : public TokenError {
public:
    explicit ExpiredTokenError(const std::string& message);
};

}  // namespace errors
}  // namespace webapp
