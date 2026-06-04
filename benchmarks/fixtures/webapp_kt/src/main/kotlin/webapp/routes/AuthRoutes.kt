package webapp.routes

import webapp.api.AuthHandlers
import webapp.middleware.AuthMiddleware
import webapp.util.getLogger

/** Wires auth endpoints to their handlers. */
class AuthRoutes(
    private val handlers: AuthHandlers,
    private val middleware: AuthMiddleware
) {
    fun register() {
        getLogger("AuthRoutes").info("registered /login, /logout")
    }

    fun dispatchLogin(email: String, password: String): String {
        return handlers.handleLogin(email, password)
    }
}
