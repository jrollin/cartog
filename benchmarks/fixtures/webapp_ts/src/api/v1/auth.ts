/** API v1 authentication endpoints. */
import { getLogger, validateRequest, sanitizeInput } from '../../utils/helpers';
import { validateLogin } from '../../validators/user';
import { AuthenticationService } from '../../services/authService';
import { DatabaseConnection } from '../../database/connection';
import { EventDispatcher } from '../../events/dispatcher';
import { ValidationError } from '../../errors';

const logger = getLogger("api.v1.auth");

/** Validate v1 auth request — name collision. */
export function validate(request: Record<string, unknown>): Record<string, unknown> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    if (!body) throw new ValidationError("Body required");
    if (body["username"] && !body["email"]) {
        body["email"] = body["username"];
    }
    return body;
}

/** Handle v1 login — entry point for deep call chain. */
export async function handleLogin(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    logger.info("API v1 login");
    const body = validate(request);
    const loginData = validateLogin(body);
    const service = new AuthenticationService(db, events);
    service.initialize();
    const ip = (request["ip"] as string) ?? "unknown";
    const result = await service.authenticate(loginData.email, loginData.password, ip);
    return { status: 200, data: result };
}

/** Handle v1 register. */
export async function handleRegister(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    logger.info("API v1 register");
    const body = validate(request);
    return { status: 201, data: body };
}
