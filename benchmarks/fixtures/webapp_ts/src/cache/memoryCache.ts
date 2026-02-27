/** In-memory LRU cache. */
import { getLogger } from '../utils/helpers';
import { BaseCache } from './base';

const logger = getLogger("cache.memory");

/** LRU memory cache. */
export class MemoryCache extends BaseCache {
    private store: Map<string, unknown> = new Map();
    private expiry: Map<string, number> = new Map();
    private maxSize: number;

    constructor(maxSize: number = 1000) {
        super("memory");
        this.maxSize = maxSize;
        logger.info(`MemoryCache created: maxSize=${maxSize}`);
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
            // Move to end for LRU
            const val = this.store.get(key);
            this.store.delete(key);
            this.store.set(key, val);
            return val ?? null;
        }
        this.misses++;
        return null;
    }

    set(key: string, value: unknown, ttl: number = 300): void {
        if (this.store.has(key)) {
            this.store.delete(key);
        } else if (this.store.size >= this.maxSize) {
            const firstKey = this.store.keys().next().value;
            if (firstKey !== undefined) {
                this.store.delete(firstKey);
                this.expiry.delete(firstKey);
                logger.info(`LRU evicted: ${firstKey}`);
            }
        }
        this.store.set(key, value);
        this.expiry.set(key, Date.now() + ttl * 1000);
    }

    delete(key: string): boolean {
        this.expiry.delete(key);
        return this.store.delete(key);
    }

    clear(): number {
        const count = this.store.size;
        this.store.clear();
        this.expiry.clear();
        return count;
    }

    size(): number {
        return this.store.size;
    }
}
