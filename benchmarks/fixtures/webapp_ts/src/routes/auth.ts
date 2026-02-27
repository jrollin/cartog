/** Authentication route handlers. */

import { getLogger, validateRequest, sanitizeInput } from '../utils/helpers';
import { AuthService } from '../auth/service';
import { validateToken, refreshToken, extractToken, TokenError } from '../auth/tokens';
import { authRequired } from '../auth/middleware';

const logger = getLogger("routes.auth");

/** Handle login requests. */
export async function loginRoute(request: Record<string, unknown>): Promise<Record<string, unknown>> {
    validateRequest(request);
    logger.info("Login route");
    const body = request["body"] as Record<string, string>;
    const email = sanitizeInput(body?.email ?? "");
    const password = body?.password ?? "";

    const service = new AuthService();
    service.initialize();
    const token = await service.login(email, password);

    if (token) {
        return { status: 200, data: { token, email } };
    }
    return { status: 401, error: "Invalid credentials" };
}

/** Handle logout requests. */
export async function logoutRoute(request: Record<string, unknown>): Promise<Record<string, unknown>> {
    const authed = authRequired(request);
    const token = extractToken(authed as { headers?: Record<string, string> });
    const service = new AuthService();
    service.initialize();
    if (token) {
        await service.logout(token);
    }
    return { status: 200, data: { message: "Logged out" } };
}

/** Handle token refresh. */
export async function refreshRoute(request: Record<string, unknown>): Promise<Record<string, unknown>> {
    validateRequest(request);
    const token = extractToken(request as { headers?: Record<string, string> });
    if (!token) {
        return { status: 401, error: "Missing token" };
    }
    try {
        const newToken = refreshToken(token);
        return { status: 200, data: { token: newToken } };
    } catch (e) {
        if (e instanceof TokenError) {
            return { status: 401, error: e.message };
        }
        throw e;
    }
}
