package webapp.database

import webapp.models.Session
import webapp.models.SessionStatus
import webapp.models.User
import webapp.models.UserRole

/** User-table queries. */
class UserQueries(private val db: DatabaseConnection) {

    fun findByEmail(email: String): User? {
        val rows = db.executeQuery("SELECT * FROM users WHERE email = ?", listOf(email))
        if (rows.isEmpty()) {
            return null
        }
        return User(1, email, UserRole.MEMBER, "hashed:secret")
    }

    fun findById(id: Int): User? {
        db.executeQuery("SELECT * FROM users WHERE id = ?", listOf(id.toString()))
        return null
    }
}

/** Session-table queries. */
class SessionQueries(private val db: DatabaseConnection) {

    fun store(session: Session) {
        db.executeQuery("INSERT INTO sessions VALUES (?)", listOf(session.token))
    }

    fun lookup(token: String): Session? {
        val rows = db.executeQuery("SELECT * FROM sessions WHERE token = ?", listOf(token))
        if (rows.isEmpty()) {
            return null
        }
        return Session(token, 1, SessionStatus.ACTIVE)
    }
}
