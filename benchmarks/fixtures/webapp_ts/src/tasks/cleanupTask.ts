/** Cleanup background tasks. */
import { getLogger } from '../utils/helpers';
import { DatabaseConnection } from '../database/connection';
import { CacheBackend } from '../cache/base';

const logger = getLogger("tasks.cleanup");

/** Clean up expired sessions. */
export async function cleanupExpiredSessions(db: DatabaseConnection): Promise<number> {
    logger.info("Cleaning up expired sessions");
    const result = await db.executeQuery("UPDATE sessions SET expiredAt = ? WHERE expiredAt IS NULL AND createdAt < ?", [Date.now(), Date.now() - 7 * 86400 * 1000]);
    logger.info(`Expired ${result.affected} sessions`);
    return result.affected;
}

/** Clean up old events. */
export async function cleanupOldEvents(db: DatabaseConnection): Promise<number> {
    logger.info("Cleaning up old events");
    const result = await db.executeQuery("DELETE FROM events WHERE processedAt IS NOT NULL AND createdAt < ?", [Date.now() - 30 * 86400 * 1000]);
    return result.affected;
}

/** Flush cache. */
export function cleanupCache(cache: CacheBackend): number {
    logger.info("Running cache cleanup");
    return cache.clear();
}

/** Run all cleanup tasks. */
export async function runAllCleanup(db: DatabaseConnection, cache: CacheBackend): Promise<Record<string, number>> {
    const sessions = await cleanupExpiredSessions(db);
    const events = await cleanupOldEvents(db);
    const cacheEntries = cleanupCache(cache);
    logger.info("Cleanup complete");
    return { expiredSessions: sessions, oldEvents: events, cacheCleared: cacheEntries };
}
