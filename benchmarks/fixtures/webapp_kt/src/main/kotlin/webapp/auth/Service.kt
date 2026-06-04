package webapp.auth

import webapp.database.DatabaseConnection
import webapp.database.UserQueries
import webapp.errors.TokenError
import webapp.models.User
import webapp.util.Logger
import webapp.util.getLogger

/** Base service with a shared logger, subclassed by concrete services. */
open class BaseService {
    val logger: Logger = getLogger("BaseService")

    fun log(message: String) {
        logger.info(message)
    }
}

/** Handles user authentication flows. */
open class AuthService(private val db: DatabaseConnection) : BaseService() {
    private val users: UserQueries = UserQueries(db)

    /** Authenticate a user with email + password, returning a token on success. */
    fun login(email: String, password: String): Token? {
        val user = findUser(email)
        if (user != null && user.checkPassword(password)) {
            log("login successful for $email")
            return generateToken(user)
        }
        log("login failed for $email")
        return null
    }

    fun logout(token: String) {
        revokeToken(token)
    }

    fun getCurrentUser(token: String): User {
        return validateToken(token)
    }

    private fun findUser(email: String): User? {
        return users.findByEmail(email)
    }
}

/** Extended auth service for admin operations. */
class AdminService(db: DatabaseConnection) : AuthService(db) {
    fun impersonate(adminToken: String, userId: Int): Token {
        val admin = getCurrentUser(adminToken)
        if (admin.isAdmin) {
            log("admin ${admin.email} impersonating $userId")
            return generateToken(admin)
        }
        throw TokenError("not authorized")
    }
}
