/** API v2 authentication endpoints — improved over v1. */
import { getLogger, validateRequest, sanitizeInput } from '../../utils/helpers';
import { validateLogin } from '../../validators/user';
import { AuthenticationService } from '../../services/authService';
import { DatabaseConnection } from '../../database/connection';
import { EventDispatcher } from '../../events/dispatcher';
import { ValidationError, AuthenticationError } from '../../errors';
import { generateToken, validateToken } from '../../auth/tokens';

const logger = getLogger("api.v2.auth");

/** Validate v2 auth request — name collision. */
export function validate(request: Record<string, unknown>): Record<string, unknown> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    if (!body) throw new ValidationError("Body required");
    if (!body["email"]) throw new ValidationError("Email required", "email");
    return body;
}

/** Handle v2 login with device tracking. */
export async function handleLogin(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    logger.info("API v2 login");
    const body = validate(request);
    const loginData = validateLogin(body);
    const service = new AuthenticationService(db, events);
    service.initialize();
    const ip = (request["ip"] as string) ?? "unknown";
    const result = await service.authenticate(loginData.email, loginData.password, ip);
    return { status: 200, data: { ...result, apiVersion: "v2" } };
}

/** Handle v2 token refresh. */
export async function handleTokenRefresh(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    logger.info("API v2 token refresh");
    validateRequest(request);
    const oldToken = (request["token"] as string) ?? "";
    if (!oldToken) throw new AuthenticationError("Refresh token required");
    const service = new AuthenticationService(db, events);
    service.initialize();
    const user = await service.verifyToken(oldToken);
    if (!user) throw new AuthenticationError("Invalid refresh token");
    const newToken = generateToken({ id: user["id"] as string, email: user["email"] as string, role: user["role"] as string });
    return { status: 200, data: { token: newToken, apiVersion: "v2" } };
}
