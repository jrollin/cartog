/** User input validation. */
import { getLogger } from '../utils/helpers';
import { ValidationError } from '../errors';
import { validateEmail, validateString } from './common';

const logger = getLogger("validators.user");

/** Validate user data — name collision with validators/payment, api/v1/auth, api/v2/auth. */
export function validate(data: Record<string, unknown>): Record<string, unknown> {
    logger.info("Validating user data");
    if (!data["email"]) throw new ValidationError("Email required", "email");
    if (!data["name"]) throw new ValidationError("Name required", "name");
    const result: Record<string, unknown> = {};
    result["email"] = validateEmail(data["email"] as string);
    result["name"] = validateString(data["name"] as string, "name", 1, 100);
    if (data["password"]) {
        const pwd = data["password"] as string;
        if (pwd.length < 8) throw new ValidationError("Password too short", "password");
        result["password"] = pwd;
    }
    if (data["role"]) {
        const allowed = ["user", "admin", "moderator"];
        if (!allowed.includes(data["role"] as string)) throw new ValidationError("Invalid role", "role");
        result["role"] = data["role"];
    }
    return result;
}

/** Validate login data. */
export function validateLogin(data: Record<string, unknown>): { email: string; password: string } {
    if (!data["email"]) throw new ValidationError("Email required", "email");
    if (!data["password"]) throw new ValidationError("Password required", "password");
    return { email: validateEmail(data["email"] as string), password: data["password"] as string };
}
