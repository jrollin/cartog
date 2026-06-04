package webapp

import webapp.api.AuthHandlers
import webapp.database.DatabaseConnection
import webapp.middleware.AuthMiddleware
import webapp.routes.AuthRoutes
import webapp.services.AuthenticationService
import webapp.util.getLogger

fun main() {
    val db = DatabaseConnection("postgres://localhost/webapp")
    db.connect()

    val service = AuthenticationService(db)
    val handlers = AuthHandlers(service)
    val middleware = AuthMiddleware()
    val routes = AuthRoutes(handlers, middleware)
    routes.register()

    val token = routes.dispatchLogin("user@example.com", "secret")
    getLogger("main").info("issued token $token")

    db.close()
}
