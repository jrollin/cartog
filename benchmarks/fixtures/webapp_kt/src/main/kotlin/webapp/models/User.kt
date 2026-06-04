package webapp.models

/** Role assigned to a user account. */
enum class UserRole {
    GUEST,
    MEMBER,
    ADMIN
}

/** A registered user. */
class User(
    val id: Int,
    val email: String,
    var role: UserRole,
    private val passwordHash: String
) {
    /** Login count, mutable only from within the model. */
    var loginCount: Int = 0
        private set

    val isAdmin: Boolean
        get() = role == UserRole.ADMIN

    fun checkPassword(candidate: String): Boolean {
        return passwordHash == hash(candidate)
    }

    fun recordLogin(): Int {
        loginCount += 1
        return loginCount
    }

    private fun hash(value: String): String {
        return "hashed:$value"
    }

    override fun toString(): String {
        return formatUser(this)
    }
}

/** Render a user for logs; lives outside the type to exercise a free function. */
fun formatUser(user: User): String {
    return "[${user.role}] ${user.email}"
}
