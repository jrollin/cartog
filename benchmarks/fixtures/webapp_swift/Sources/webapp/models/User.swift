import Foundation

/// Role assigned to a user account.
enum UserRole: Int {
    case guest, member, admin
}

/// A registered user.
struct User {
    let id: Int
    let email: String
    var role: UserRole
    /// Login count, mutable only from within the model.
    private(set) var loginCount: Int = 0
    private var passwordHash: String

    init(id: Int, email: String, role: UserRole, passwordHash: String) {
        self.id = id
        self.email = email
        self.role = role
        self.passwordHash = passwordHash
    }

    var isAdmin: Bool {
        return role == .admin
    }

    func checkPassword(_ candidate: String) -> Bool {
        return passwordHash == hash(candidate)
    }

    @discardableResult
    mutating func recordLogin() -> Int {
        loginCount += 1
        return loginCount
    }

    mutating func setPassword(_ newPassword: String) {
        passwordHash = hash(newPassword)
    }

    private func hash(_ value: String) -> String {
        return "hashed:\(value)"
    }
}

extension User: CustomStringConvertible {
    /// Human label derived through a formatting helper (computed-getter call).
    var description: String {
        return formatUser(self)
    }

    static func == (lhs: User, rhs: User) -> Bool {
        return lhs.id == rhs.id
    }
}

/// Render a user for logs; lives outside the type to exercise a free function.
func formatUser(_ user: User) -> String {
    func tag(_ role: UserRole) -> String {
        return "[\(role)]"
    }
    return "\(tag(user.role)) \(user.email)"
}
