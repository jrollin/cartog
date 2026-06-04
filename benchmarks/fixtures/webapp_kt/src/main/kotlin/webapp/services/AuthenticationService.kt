package webapp.services

import webapp.auth.AuthService
import webapp.auth.BaseService
import webapp.auth.Token
import webapp.database.DatabaseConnection
import webapp.database.SessionQueries
import webapp.database.UserQueries
import webapp.errors.AuthenticationError
import webapp.models.Session
import webapp.models.SessionStatus
import webapp.models.User

/**
 * Orchestrates the login flow across auth, queries, and sessions.
 * Entry point of the deep call chain: authenticate -> verifyCredentials ->
 * loadUser -> UserQueries.findByEmail -> DatabaseConnection.executeQuery.
 */
class AuthenticationService(db: DatabaseConnection) : BaseService() {
    private val auth: AuthService = AuthService(db)
    private val users: UserQueries = UserQueries(db)
    private val sessions: SessionQueries = SessionQueries(db)

    /** Main entry point for the login flow. */
    fun authenticate(email: String, password: String): Token {
        log("authenticating $email")
        val user = verifyCredentials(email, password)
        val token = auth.login(user.email, password)
            ?: throw AuthenticationError("login failed")
        persistSession(token)
        return token
    }

    private fun verifyCredentials(email: String, password: String): User {
        val user = loadUser(email)
        if (!user.checkPassword(password)) {
            throw AuthenticationError("bad credentials")
        }
        return user
    }

    private fun loadUser(email: String): User {
        return users.findByEmail(email)
            ?: throw AuthenticationError("no such user")
    }

    private fun persistSession(token: Token) {
        sessions.store(Session(token.value, token.userId, SessionStatus.ACTIVE))
    }
}
