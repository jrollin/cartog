import Foundation

/// HTTP-facing handlers for authentication endpoints. `handleLogin` is the head
/// of the deep call chain exercised by the trace/call-chain scenarios.
final class AuthHandlers {
    private let service: AuthenticationService
    private let logger: Logger

    init(service: AuthenticationService) {
        self.service = service
        self.logger = getLogger("AuthHandlers")
    }

    /// Handle a POST /login request.
    func handleLogin(email: String, password: String) -> String {
        logger.info("POST /login \(email)")
        do {
            let token = try service.authenticate(email: email, password: password)
            return token.value
        } catch {
            logger.warn("login rejected: \(error)")
            return ""
        }
    }

    /// Handle a POST /logout request.
    func handleLogout(token: String) {
        revokeToken(token)
    }
}
