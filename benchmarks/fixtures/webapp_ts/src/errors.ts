/** Application error hierarchy. */

/** Base application error. */
export class AppError extends Error {
    public readonly code: number;

    constructor(message: string, code: number = 500) {
        super(message);
        this.name = "AppError";
        this.code = code;
    }

    /** Serialize to plain object. */
    toJSON(): Record<string, unknown> {
        return { error: this.name, message: this.message, code: this.code };
    }
}

/** Raised when input validation fails. */
export class ValidationError extends AppError {
    public readonly field: string | null;

    constructor(message: string, field: string | null = null) {
        super(message, 400);
        this.name = "ValidationError";
        this.field = field;
    }
}

/** Raised when a payment operation fails. */
export class PaymentError extends AppError {
    public readonly transactionId: string | null;

    constructor(message: string, transactionId: string | null = null) {
        super(message, 402);
        this.name = "PaymentError";
        this.transactionId = transactionId;
    }
}

/** Raised when a resource is not found. */
export class NotFoundError extends AppError {
    public readonly resource: string;
    public readonly identifier: string;

    constructor(resource: string, identifier: string) {
        super(`${resource} with id '${identifier}' not found`, 404);
        this.name = "NotFoundError";
        this.resource = resource;
        this.identifier = identifier;
    }
}

/** Raised when rate limit is exceeded. */
export class RateLimitError extends AppError {
    public readonly retryAfter: number;

    constructor(retryAfter: number = 60) {
        super(`Rate limit exceeded. Retry after ${retryAfter}s`, 429);
        this.name = "RateLimitError";
        this.retryAfter = retryAfter;
    }
}

/** Raised when authentication fails. */
export class AuthenticationError extends AppError {
    constructor(message: string = "Authentication required") {
        super(message, 401);
        this.name = "AuthenticationError";
    }
}

/** Raised when authorization fails. */
export class AuthorizationError extends AppError {
    constructor(action: string, resource: string) {
        super(`Not authorized to ${action} on ${resource}`, 403);
        this.name = "AuthorizationError";
    }
}

/** Raised when a database operation fails. */
export class DatabaseError extends AppError {
    public readonly query: string | null;

    constructor(message: string, query: string | null = null) {
        super(message, 500);
        this.name = "DatabaseError";
        this.query = query;
    }
}
