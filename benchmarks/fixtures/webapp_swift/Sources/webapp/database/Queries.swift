import Foundation

/// User-table queries.
final class UserQueries {
    private let db: DatabaseConnection

    init(db: DatabaseConnection) {
        self.db = db
    }

    func findByEmail(_ email: String) -> User? {
        let rows = db.executeQuery("SELECT * FROM users WHERE email = '\(email)'")
        if rows.isEmpty {
            return nil
        }
        return User(id: 1, email: email, role: .member, passwordHash: "hashed:secret")
    }

    func findById(_ id: Int) -> User? {
        _ = db.executeQuery("SELECT * FROM users WHERE id = \(id)")
        return nil
    }
}

/// Session-table queries.
final class SessionQueries {
    private let db: DatabaseConnection

    init(db: DatabaseConnection) {
        self.db = db
    }

    func store(_ session: Session) {
        _ = db.executeQuery("INSERT INTO sessions VALUES ('\(session.token)')")
    }

    func lookup(_ token: String) -> Session? {
        let rows = db.executeQuery("SELECT * FROM sessions WHERE token = '\(token)'")
        if rows.isEmpty {
            return nil
        }
        return Session(token: token, userId: 1, status: .active)
    }
}
