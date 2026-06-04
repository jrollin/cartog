package webapp.errors

/** Base application error. */
class AppError(val message: String) : Exception(message)

/** Raised when authentication fails. */
class AuthenticationError(val message: String) : Exception(message)

/** Raised for token validation/expiry problems. */
class TokenError(val message: String) : Exception(message)

/** Raised when input validation fails. */
class ValidationError(val field: String, val reason: String) : Exception(reason)
