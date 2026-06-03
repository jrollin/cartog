import Foundation

/// Rejects requests without a valid token before they reach a handler.
final class AuthMiddleware {
    private let logger: Logger

    init() {
        self.logger = getLogger("AuthMiddleware")
    }

    func authorize(token: String) -> User? {
        do {
            return try validateToken(token)
        } catch {
            logger.warn("unauthorized: \(error)")
            return nil
        }
    }
}

/// Token-bucket rate limiter.
final class RateLimitMiddleware {
    private var counts: [String: Int] = [:]
    private let limit: Int

    init(limit: Int) {
        self.limit = limit
    }

    func allow(key: String) -> Bool {
        let n = (counts[key] ?? 0) + 1
        counts[key] = n
        return n <= limit
    }
}
