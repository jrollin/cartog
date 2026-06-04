package webapp.util

/** Severity level for log records. */
enum class LogLevel {
    DEBUG,
    INFO,
    WARN,
    ERROR
}

/** Simple stdout logger shared across services. */
class Logger(val name: String) {

    fun info(message: String) {
        emit(LogLevel.INFO, message)
    }

    fun warn(message: String) {
        emit(LogLevel.WARN, message)
    }

    fun error(message: String) {
        emit(LogLevel.ERROR, message)
    }

    private fun emit(level: LogLevel, message: String) {
        println("[$name] $message")
    }
}

/**
 * Return a logger tagged with the caller's component name. High-fanout helper:
 * nearly every service obtains its logger through this function.
 */
fun getLogger(component: String): Logger {
    return Logger(component)
}
