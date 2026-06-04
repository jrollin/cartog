import Foundation

/// Base application error.
struct AppError: Error {
    let message: String
}

/// Raised when authentication fails.
struct AuthenticationError: Error {
    let message: String
}

/// Raised for token validation/expiry problems.
struct TokenError: Error {
    let message: String
}

/// Raised when input validation fails.
struct ValidationError: Error {
    let field: String
    let message: String
}
