/** Cacheable service with built-in caching. */
import { getLogger } from '../utils/helpers';
import { BaseService } from '../auth/service';

const logger = getLogger("services.cacheable");

/** Service with caching support. */
export class CacheableService extends BaseService {
    private cache: Map<string, { value: unknown; expiry: number }> = new Map();
    private defaultTtl: number = 300;

    constructor(serviceName: string = "cacheable") {
        super(serviceName);
    }

    /** Get value from cache. */
    cacheGet(key: string): unknown | null {
        const entry = this.cache.get(key);
        if (entry && Date.now() < entry.expiry) {
            logger.info(`Cache hit: ${key}`);
            return entry.value;
        }
        if (entry) this.cache.delete(key);
        logger.info(`Cache miss: ${key}`);
        return null;
    }

    /** Set value in cache. */
    cacheSet(key: string, value: unknown, ttl?: number): void {
        const effectiveTtl = ttl ?? this.defaultTtl;
        this.cache.set(key, { value, expiry: Date.now() + effectiveTtl * 1000 });
        logger.info(`Cache set: ${key} (ttl=${effectiveTtl}s)`);
    }

    /** Invalidate a cache entry. */
    cacheInvalidate(key: string): boolean {
        return this.cache.delete(key);
    }

    /** Clear all cache entries. */
    cacheClear(): number {
        const count = this.cache.size;
        this.cache.clear();
        logger.info(`Cache cleared: ${count} entries`);
        return count;
    }
}
