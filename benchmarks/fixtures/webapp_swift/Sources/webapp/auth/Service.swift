import Foundation

/// Base service with a shared logger, subclassed by concrete services.
class BaseService {
    let logger: Logger

    init() {
        logger = getLogger(String(describing: type(of: self)))
    }

    func log(_ message: String) {
        logger.info(message)
    }
}

/// Handles user authentication flows.
class AuthService: BaseService {
    private let db: DatabaseConnection
    private let users: UserQueries

    init(db: DatabaseConnection) {
        self.db = db
        self.users = UserQueries(db: db)
        super.init()
    }

    /// Authenticate a user with email + password, returning a token on success.
    func login(email: String, password: String) -> Token? {
        let user = findUser(email)
        if let user = user, user.checkPassword(password) {
            log("login successful for \(email)")
            return generateToken(user)
        }
        log("login failed for \(email)")
        return nil
    }

    func logout(token: String) {
        revokeToken(token)
    }

    func getCurrentUser(token: String) throws -> User {
        return try validateToken(token)
    }

    private func findUser(_ email: String) -> User? {
        return users.findByEmail(email)
    }
}

/// Extended auth service for admin operations.
class AdminService: AuthService {
    func impersonate(adminToken: String, userId: Int) throws -> Token {
        let admin = try getCurrentUser(token: adminToken)
        if admin.isAdmin {
            log("admin \(admin.email) impersonating \(userId)")
            return generateToken(admin)
        }
        throw TokenError(message: "not authorized")
    }
}
