/** Rate limiting middleware. */
import { getLogger, validateRequest } from '../utils/helpers';
import { RateLimitError } from '../errors';
import { CacheBackend } from '../cache/base';

const logger = getLogger("middleware.rateLimit");

/** Rate limiter. */
export class RateLimiter {
    constructor(private cache: CacheBackend, private limit: number = 100, private window: number = 60) {}

    check(key: string): { allowed: boolean; remaining: number } {
        const cacheKey = `ratelimit:${key}`;
        const current = this.cache.get(cacheKey) as number | null;
        if (current === null) {
            this.cache.set(cacheKey, 1, this.window);
            return { allowed: true, remaining: this.limit - 1 };
        }
        if (current >= this.limit) {
            logger.info(`Rate limit exceeded: ${key}`);
            return { allowed: false, remaining: 0 };
        }
        this.cache.set(cacheKey, current + 1, this.window);
        return { allowed: true, remaining: this.limit - current - 1 };
    }
}

/** Apply rate limiting. */
export function rateLimitMiddleware(request: Record<string, unknown>, cache: CacheBackend): Record<string, unknown> {
    validateRequest(request);
    const ip = (request["ip"] as string) ?? "unknown";
    const path = (request["path"] as string) ?? "/";
    const limiter = new RateLimiter(cache);
    const result = limiter.check(`${ip}:${path}`);
    if (!result.allowed) throw new RateLimitError(60);
    return { ...request, rateLimit: result };
}
