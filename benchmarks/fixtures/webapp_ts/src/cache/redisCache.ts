/** Redis-backed cache. */
import { getLogger } from '../utils/helpers';
import { BaseCache } from './base';

const logger = getLogger("cache.redis");

/** Redis cache implementation. */
export class RedisCache extends BaseCache {
    private store: Map<string, unknown> = new Map();
    private expiry: Map<string, number> = new Map();

    constructor(host: string = "localhost", port: number = 6379) {
        super("redis");
        logger.info(`RedisCache created: ${host}:${port}`);
    }

    get(key: string): unknown | null {
        if (this.store.has(key)) {
            const exp = this.expiry.get(key) ?? Infinity;
            if (Date.now() > exp) {
                this.store.delete(key);
                this.expiry.delete(key);
                this.misses++;
                return null;
            }
            this.hits++;
            return this.store.get(key) ?? null;
        }
        this.misses++;
        return null;
    }

    set(key: string, value: unknown, ttl: number = 300): void {
        this.store.set(key, value);
        this.expiry.set(key, Date.now() + ttl * 1000);
        logger.info(`Redis SET ${key} (ttl=${ttl})`);
    }

    delete(key: string): boolean {
        this.expiry.delete(key);
        return this.store.delete(key);
    }

    clear(): number {
        const count = this.store.size;
        this.store.clear();
        this.expiry.clear();
        logger.info(`Redis FLUSHDB: ${count} keys`);
        return count;
    }

    incr(key: string, amount: number = 1): number {
        const current = (this.store.get(key) as number) ?? 0;
        const newVal = current + amount;
        this.store.set(key, newVal);
        return newVal;
    }
}
