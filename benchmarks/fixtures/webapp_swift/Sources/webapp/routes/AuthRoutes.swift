import Foundation

/// Wires auth endpoints to their handlers.
final class AuthRoutes {
    private let handlers: AuthHandlers
    private let middleware: AuthMiddleware

    init(handlers: AuthHandlers, middleware: AuthMiddleware) {
        self.handlers = handlers
        self.middleware = middleware
    }

    func register() {
        getLogger("AuthRoutes").info("registered /login, /logout")
    }

    func dispatchLogin(email: String, password: String) -> String {
        return handlers.handleLogin(email: email, password: password)
    }
}
