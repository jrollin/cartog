/** Auth middleware. */
import { getLogger, validateRequest } from '../utils/helpers';
import { validateToken, extractToken, TokenError } from '../auth/tokens';
import { AuthenticationError } from '../errors';

const logger = getLogger("middleware.auth");

/** Authentication middleware. */
export function authMiddleware(request: Record<string, unknown>): Record<string, unknown> {
    validateRequest(request);
    const publicPaths = ["/health", "/login", "/register"];
    if (publicPaths.includes(request["path"] as string)) return request;
    const token = extractToken(request as { headers?: Record<string, string> });
    if (!token) throw new AuthenticationError("Missing token");
    try {
        const claims = validateToken(token);
        return { ...request, user: claims, authenticated: true };
    } catch (e) {
        if (e instanceof TokenError) logger.warn("Token validation failed");
        throw new AuthenticationError("Invalid token");
    }
}
