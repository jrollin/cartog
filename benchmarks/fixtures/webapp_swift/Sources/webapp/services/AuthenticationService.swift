import Foundation

/// Orchestrates the login flow across auth, queries, and sessions.
/// Entry point of the deep call chain: authenticate → verifyCredentials →
/// loadUser → UserQueries.findByEmail → DatabaseConnection.execute.
class AuthenticationService: BaseService {
    private let auth: AuthService
    private let users: UserQueries
    private let sessions: SessionQueries

    init(db: DatabaseConnection) {
        self.auth = AuthService(db: db)
        self.users = UserQueries(db: db)
        self.sessions = SessionQueries(db: db)
        super.init()
    }

    /// Main entry point for the login flow.
    func authenticate(email: String, password: String) throws -> Token {
        log("authenticating \(email)")
        let user = try verifyCredentials(email: email, password: password)
        let token = auth.login(email: user.email, password: password)
        guard let token = token else {
            throw AuthenticationError(message: "login failed")
        }
        persistSession(token)
        return token
    }

    private func verifyCredentials(email: String, password: String) throws -> User {
        let user = try loadUser(email)
        if !user.checkPassword(password) {
            throw AuthenticationError(message: "bad credentials")
        }
        return user
    }

    private func loadUser(_ email: String) throws -> User {
        guard let user = users.findByEmail(email) else {
            throw AuthenticationError(message: "no such user")
        }
        return user
    }

    private func persistSession(_ token: Token) {
        sessions.store(Session(token: token.value, userId: token.userId, status: .active))
    }
}
