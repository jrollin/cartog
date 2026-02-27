/** Application entry point. */
import { getLogger } from './utils/helpers';
import { loadConfig, validateConfig } from './config';
import { ConnectionPool } from './database/pool';
import { DatabaseConnection } from './database/connection';
import { MigrationRunner } from './database/migrations';
import { EventDispatcher } from './events/dispatcher';
import { registerDefaultHandlers } from './events/handlers';
import { RedisCache } from './cache/redisCache';

const logger = getLogger("app");

/** Initialize the application. */
export async function initializeApp(): Promise<{
    db: DatabaseConnection;
    events: EventDispatcher;
    cache: RedisCache;
}> {
    logger.info("Initializing application");
    const config = loadConfig();
    if (!validateConfig(config)) {
        throw new Error("Invalid configuration");
    }

    // Database
    const pool = new ConnectionPool(config.dbDsn);
    pool.initialize();
    const db = new DatabaseConnection(pool);

    // Migrations
    const migrations = new MigrationRunner(db);
    await migrations.runPending();

    // Events
    const events = new EventDispatcher();
    registerDefaultHandlers(events);

    // Cache
    const cache = new RedisCache(config.redisHost, config.redisPort);

    logger.info("Application initialized");
    return { db, events, cache };
}
