/** Token management: generation, validation, and lifecycle. */

import { getLogger } from '../utils/helpers';

const logger = getLogger("auth.tokens");

/** Token expiry in seconds. */
export const TOKEN_EXPIRY = 3600;

/** Refresh token expiry. */
export const REFRESH_TOKEN_EXPIRY = 86400 * 7;

/** Error thrown when token operations fail. */
export class TokenError extends Error {
    constructor(message: string) {
        super(message);
        this.name = "TokenError";
    }
}

/** Error for expired tokens. */
export class ExpiredTokenError extends TokenError {
    constructor() {
        super("Token has expired");
        this.name = "ExpiredTokenError";
    }
}

/** Error for invalid token scope. */
export class InvalidScopeError extends TokenError {
    constructor(scope: string) {
        super(`Invalid token scope: ${scope}`);
        this.name = "InvalidScopeError";
    }
}

/** Token claims interface. */
export interface TokenClaims {
    userId: string;
    email: string;
    role: string;
    exp: number;
}

/** Generate a new authentication token for a user. */
export function generateToken(user: { id: string; email: string; role: string }): string {
    logger.info(`Generating token for user ${user.id}`);
    const payload = {
        userId: user.id,
        email: user.email,
        role: user.role,
        exp: Date.now() + TOKEN_EXPIRY * 1000,
    };
    // Simulated token generation
    return Buffer.from(JSON.stringify(payload)).toString("base64");
}

/** Validate a token and return its claims. */
export function validateToken(token: string): TokenClaims {
    logger.info("Validating token");
    if (!token || token.length < 10) {
        throw new TokenError("Invalid token format");
    }
    try {
        const decoded = JSON.parse(Buffer.from(token, "base64").toString());
        if (decoded.exp && decoded.exp < Date.now()) {
            throw new ExpiredTokenError();
        }
        return decoded as TokenClaims;
    } catch (e) {
        if (e instanceof TokenError) throw e;
        throw new TokenError("Token decode failed");
    }
}

/** Refresh an existing token. */
export function refreshToken(oldToken: string): string {
    logger.info("Refreshing token");
    const claims = validateToken(oldToken);
    return generateToken({ id: claims.userId, email: claims.email, role: claims.role });
}

/** Revoke a single token. */
export function revokeToken(token: string): boolean {
    logger.info("Revoking token");
    // In real impl, would add to blacklist
    validateToken(token);
    return true;
}

/** Revoke all tokens for a user. */
export function revokeAllTokens(userId: string): number {
    logger.info(`Revoking all tokens for user ${userId}`);
    // In real impl, would invalidate all user sessions
    return 1;
}

/** Extract token from Authorization header. */
export function extractToken(request: { headers?: Record<string, string> }): string | null {
    const auth = request.headers?.["Authorization"] ?? "";
    if (auth.startsWith("Bearer ")) {
        return auth.substring(7);
    }
    return null;
}

/** Find a session by its token. */
export function findByToken(token: string): { userId: string; active: boolean } | null {
    logger.info("Looking up session by token");
    try {
        const claims = validateToken(token);
        return { userId: claims.userId, active: true };
    } catch {
        return null;
    }
}
