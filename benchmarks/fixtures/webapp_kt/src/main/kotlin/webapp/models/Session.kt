package webapp.models

/** Lifecycle state of a session. */
enum class SessionStatus {
    ACTIVE,
    EXPIRED,
    REVOKED
}

/** A login session bound to a user. */
data class Session(
    val token: String,
    val userId: Int,
    var status: SessionStatus
) {
    val isActive: Boolean
        get() = status == SessionStatus.ACTIVE
}
