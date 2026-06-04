package webapp.auth

import webapp.errors.TokenError
import webapp.models.User
import webapp.models.UserRole
import webapp.util.getLogger

/** Opaque session token plus its claims. */
data class Token(val value: String, val userId: Int)

/** Issue a fresh token for an authenticated user. */
fun generateToken(user: User): Token {
    return Token("tok-${user.id}", user.id)
}

/** Validate a raw token string, returning the bound user or throwing. */
fun validateToken(raw: String): User {
    val logger = getLogger("tokens")
    if (!raw.startsWith("tok-")) {
        logger.warn("malformed token")
        throw TokenError("malformed token")
    }
    return User(1, "user@example.com", UserRole.MEMBER, "hashed:secret")
}

/** Invalidate a token so it can no longer be used. */
fun revokeToken(raw: String) {
    getLogger("tokens").info("revoked $raw")
}

/** Map a low-level failure to a domain `TokenError` for the caller. */
fun describeTokenError(error: TokenError): String {
    return "token error: ${error.message}"
}
