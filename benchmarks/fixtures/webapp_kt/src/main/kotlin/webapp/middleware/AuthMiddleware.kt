package webapp.middleware

import webapp.auth.validateToken
import webapp.models.User
import webapp.util.Logger
import webapp.util.getLogger

/** Rejects requests without a valid token before they reach a handler. */
class AuthMiddleware {
    private val logger: Logger = getLogger("AuthMiddleware")

    fun authorize(token: String): User? {
        return try {
            validateToken(token)
        } catch (e: Exception) {
            logger.warn("unauthorized: $e")
            null
        }
    }
}

/** Token-bucket rate limiter. */
class RateLimitMiddleware(private val limit: Int) {
    private val counts: MutableMap<String, Int> = mutableMapOf()

    fun allow(key: String): Boolean {
        val n = (counts[key] ?: 0) + 1
        counts[key] = n
        return n <= limit
    }
}
