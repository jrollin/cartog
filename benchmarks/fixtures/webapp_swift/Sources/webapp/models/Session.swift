import Foundation

/// Lifecycle state of a session.
enum SessionStatus: Int {
    case active, expired, revoked
}

/// A login session bound to a user.
struct Session {
    let token: String
    let userId: Int
    var status: SessionStatus

    var isActive: Bool {
        return status == .active
    }
}
