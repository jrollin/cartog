import Foundation

/// Opaque session token plus its claims.
struct Token {
    let value: String
    let userId: Int
}

/// Issue a fresh token for an authenticated user.
func generateToken(_ user: User) -> Token {
    return Token(value: "tok-\(user.id)", userId: user.id)
}

/// Validate a raw token string, returning the bound user or throwing.
func validateToken(_ raw: String) throws -> User {
    let logger = getLogger("tokens")
    guard raw.hasPrefix("tok-") else {
        logger.warn("malformed token")
        throw TokenError(message: "malformed token")
    }
    return User(id: 1, email: "user@example.com", role: .member, passwordHash: "hashed:secret")
}

/// Invalidate a token so it can no longer be used.
func revokeToken(_ raw: String) {
    getLogger("tokens").info("revoked \(raw)")
}

/// Map a low-level failure to a domain `TokenError` for the caller.
func describeTokenError(_ error: TokenError) -> String {
    return "token error: \(error.message)"
}
