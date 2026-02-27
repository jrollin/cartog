/** Shared utility helpers. */

/** Simple logger interface. */
export interface Logger {
    info(msg: string): void;
    error(msg: string): void;
    warn(msg: string): void;
}

/** Get a named logger instance. */
export function getLogger(name: string): Logger {
    return {
        info: (msg: string) => console.log(`[${name}] INFO: ${msg}`),
        error: (msg: string) => console.error(`[${name}] ERROR: ${msg}`),
        warn: (msg: string) => console.warn(`[${name}] WARN: ${msg}`),
    };
}

/** Validate that a request object has required fields. */
export function validateRequest(request: Record<string, unknown>): boolean {
    if (!request || typeof request !== "object") {
        throw new Error("Request must be an object");
    }
    const required = ["method", "path"];
    for (const field of required) {
        if (!(field in request)) {
            throw new Error(`Missing required field: ${field}`);
        }
    }
    return true;
}

/** Generate a unique request identifier. */
export function generateRequestId(): string {
    const ts = Date.now();
    const rand = Math.random().toString(36).substring(2, 8);
    return `req-${ts}-${rand}`;
}

/** Sanitize user input by removing control characters. */
export function sanitizeInput(value: string): string {
    if (!value) return "";
    return value.replace(/[\x00-\x1f]/g, "").trim();
}

/** Paginate a list of items. */
export function paginate<T>(items: T[], page: number = 1, perPage: number = 20): {
    items: T[];
    page: number;
    perPage: number;
    total: number;
    pages: number;
} {
    const total = items.length;
    const start = (page - 1) * perPage;
    const pageItems = items.slice(start, start + perPage);
    return {
        items: pageItems,
        page,
        perPage,
        total,
        pages: Math.ceil(total / perPage),
    };
}

/** Mask sensitive fields in an object for logging. */
export function maskSensitive(data: Record<string, unknown>, fields: string[]): Record<string, unknown> {
    const masked = { ...data };
    for (const field of fields) {
        if (field in masked) {
            const val = String(masked[field]);
            masked[field] = val.length > 4 ? val.slice(0, 2) + "***" + val.slice(-2) : "***";
        }
    }
    return masked;
}

/** Retry an async operation with exponential backoff. */
export async function retryOperation<T>(
    fn: () => Promise<T>,
    maxRetries: number = 3,
    delay: number = 1000
): Promise<T> {
    let lastError: Error | null = null;
    for (let attempt = 0; attempt < maxRetries; attempt++) {
        try {
            return await fn();
        } catch (e) {
            lastError = e as Error;
            await new Promise(resolve => setTimeout(resolve, delay * Math.pow(2, attempt)));
        }
    }
    throw lastError;
}
