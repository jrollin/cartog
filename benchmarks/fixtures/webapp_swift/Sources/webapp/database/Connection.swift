import Foundation

/// Owns a live database connection and runs queries.
final class DatabaseConnection {
    let dsn: String
    private let logger: Logger

    init(dsn: String) {
        self.dsn = dsn
        self.logger = getLogger("DatabaseConnection")
    }

    func connect() {
        logger.info("connecting to \(dsn)")
    }

    /// Run a SQL statement on the live handle. Leaf of the login call chain.
    func executeQuery(_ query: String) -> [String] {
        let handle = getConnection()
        logger.info("execute on \(handle): \(query)")
        return []
    }

    /// Acquire (or reuse) the underlying socket handle.
    private func getConnection() -> String {
        return "conn:\(dsn)"
    }

    func close() {
        logger.info("closing \(dsn)")
    }
}
