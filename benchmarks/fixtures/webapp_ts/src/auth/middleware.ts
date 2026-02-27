/** Authentication middleware. */

import { getLogger, validateRequest } from '../utils/helpers';
import { validateToken, extractToken, TokenError } from './tokens';
import { AuthenticationError, AuthorizationError } from '../errors';

const logger = getLogger("auth.middleware");

const PUBLIC_PATHS = ["/health", "/login", "/register", "/docs"];

/** Verify authentication and attach user context. */
export function authRequired(request: Record<string, unknown>): Record<string, unknown> {
    validateRequest(request);
    const path = request["path"] as string;
    if (PUBLIC_PATHS.includes(path)) {
        return request;
    }
    const token = extractToken(request as { headers?: Record<string, string> });
    if (!token) {
        throw new AuthenticationError("Missing authentication token");
    }
    try {
        const claims = validateToken(token);
        return { ...request, user: claims, authenticated: true };
    } catch (e) {
        if (e instanceof TokenError) {
            logger.warn("Token validation failed in middleware");
        }
        throw new AuthenticationError("Invalid or expired token");
    }
}

/** Require a specific role. */
export function requireRole(request: Record<string, unknown>, requiredRole: string): void {
    const user = request["user"] as { role?: string } | undefined;
    const userRole = user?.role ?? "user";
    const hierarchy: Record<string, number> = { admin: 3, moderator: 2, user: 1 };
    if ((hierarchy[userRole] ?? 0) < (hierarchy[requiredRole] ?? 0)) {
        throw new AuthorizationError(requiredRole, request["path"] as string);
    }
}
