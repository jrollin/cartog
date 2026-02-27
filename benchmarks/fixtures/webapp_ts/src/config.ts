/** Application configuration. */
import { getLogger } from './utils/helpers';

const logger = getLogger("config");

export interface AppConfig {
    port: number;
    host: string;
    dbDsn: string;
    redisHost: string;
    redisPort: number;
    jwtSecret: string;
    environment: string;
    logLevel: string;
    rateLimitPerMinute: number;
    corsOrigins: string[];
}

/** Load configuration from environment. */
export function loadConfig(): AppConfig {
    logger.info("Loading configuration");
    return {
        port: parseInt(process.env["PORT"] ?? "3000", 10),
        host: process.env["HOST"] ?? "0.0.0.0",
        dbDsn: process.env["DATABASE_URL"] ?? "sqlite://app.db",
        redisHost: process.env["REDIS_HOST"] ?? "localhost",
        redisPort: parseInt(process.env["REDIS_PORT"] ?? "6379", 10),
        jwtSecret: process.env["JWT_SECRET"] ?? "dev-secret",
        environment: process.env["NODE_ENV"] ?? "development",
        logLevel: process.env["LOG_LEVEL"] ?? "info",
        rateLimitPerMinute: parseInt(process.env["RATE_LIMIT"] ?? "100", 10),
        corsOrigins: (process.env["CORS_ORIGINS"] ?? "http://localhost:3000").split(","),
    };
}

/** Validate configuration. */
export function validateConfig(config: AppConfig): boolean {
    if (config.port < 1 || config.port > 65535) {
        logger.error(`Invalid port: ${config.port}`);
        return false;
    }
    if (!config.dbDsn) {
        logger.error("Database DSN is required");
        return false;
    }
    if (config.environment === "production" && config.jwtSecret === "dev-secret") {
        logger.warn("Using dev JWT secret in production!");
    }
    return true;
}
