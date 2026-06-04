package webapp.api

import webapp.auth.revokeToken
import webapp.services.AuthenticationService
import webapp.util.Logger
import webapp.util.getLogger

/**
 * HTTP-facing handlers for authentication endpoints. `handleLogin` is the head
 * of the deep call chain exercised by the trace/call-chain scenarios.
 */
class AuthHandlers(private val service: AuthenticationService) {
    private val logger: Logger = getLogger("AuthHandlers")

    /** Handle a POST /login request. */
    fun handleLogin(email: String, password: String): String {
        logger.info("POST /login $email")
        return try {
            val token = service.authenticate(email, password)
            token.value
        } catch (e: Exception) {
            logger.warn("login rejected: $e")
            ""
        }
    }

    /** Handle a POST /logout request. */
    fun handleLogout(token: String) {
        revokeToken(token)
    }
}
