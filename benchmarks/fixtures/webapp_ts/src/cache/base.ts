/** Cache interface and base class. */

/** Cache interface. */
export interface CacheBackend {
    get(key: string): unknown | null;
    set(key: string, value: unknown, ttl?: number): void;
    delete(key: string): boolean;
    clear(): number;
}

/** Base cache with stats tracking. */
export abstract class BaseCache implements CacheBackend {
    protected name: string;
    protected hits: number = 0;
    protected misses: number = 0;

    constructor(name: string) {
        this.name = name;
    }

    abstract get(key: string): unknown | null;
    abstract set(key: string, value: unknown, ttl?: number): void;
    abstract delete(key: string): boolean;
    abstract clear(): number;

    stats(): { backend: string; hits: number; misses: number; hitRate: string } {
        const total = this.hits + this.misses;
        const rate = total > 0 ? (this.hits / total * 100) : 0;
        return { backend: this.name, hits: this.hits, misses: this.misses, hitRate: `${rate.toFixed(1)}%` };
    }
}
