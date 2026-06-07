package webapp.errors

/** Base application error. */
class AppError(override val message: String) : Exception(message)

/** Raised when authentication fails. */
class AuthenticationError(override val message: String) : Exception(message)

/** Raised for token validation/expiry problems. */
class TokenError(override val message: String) : Exception(message)

/** Raised when input validation fails. */
class ValidationError(val field: String, val reason: String) : Exception(reason)
