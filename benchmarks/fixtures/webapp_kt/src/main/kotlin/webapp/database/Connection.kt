package webapp.database

import webapp.util.Logger
import webapp.util.getLogger

/** Owns a live database connection and runs queries. */
class DatabaseConnection(val dsn: String) {
    private val logger: Logger = getLogger("DatabaseConnection")

    fun connect() {
        logger.info("connecting to $dsn")
    }

    /**
     * Run a parameterized SQL statement on the live handle. Bound values are
     * passed separately so user data is never interpolated into the query text.
     * Leaf of the login call chain.
     */
    fun executeQuery(query: String, args: List<String> = emptyList()): List<String> {
        val handle = getConnection()
        logger.info("execute on $handle: $query (${args.size} args)")
        return emptyList()
    }

    /** Acquire (or reuse) the underlying socket handle. */
    private fun getConnection(): String {
        return "conn:$dsn"
    }

    fun close() {
        logger.info("closing $dsn")
    }
}
