import Foundation

/// Severity level for log records.
enum LogLevel: Int {
    case debug, info, warn, error
}

/// Simple stdout logger shared across services.
final class Logger {
    let name: String

    init(name: String) {
        self.name = name
    }

    func info(_ message: String) {
        emit(.info, message)
    }

    func warn(_ message: String) {
        emit(.warn, message)
    }

    func error(_ message: String) {
        emit(.error, message)
    }

    private func emit(_ level: LogLevel, _ message: String) {
        print("[\(name)] \(message)")
    }
}

/// Return a logger tagged with the caller's component name. High-fanout helper:
/// nearly every service obtains its logger through this function.
func getLogger(_ component: String) -> Logger {
    return Logger(name: component)
}
