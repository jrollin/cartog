/** Common validation utilities. */
import { getLogger } from '../utils/helpers';
import { ValidationError } from '../errors';

const logger = getLogger("validators.common");

const EMAIL_REGEX = /^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/;

/** Validate email format. */
export function validateEmail(email: string): string {
    if (!email) throw new ValidationError("Email is required", "email");
    const clean = email.trim().toLowerCase();
    if (!EMAIL_REGEX.test(clean)) throw new ValidationError(`Invalid email: ${email}`, "email");
    return clean;
}

/** Validate string length. */
export function validateString(value: string, field: string, minLen: number = 1, maxLen: number = 255): string {
    if (!value) throw new ValidationError(`${field} is required`, field);
    const stripped = value.trim();
    if (stripped.length < minLen) throw new ValidationError(`${field} too short`, field);
    if (stripped.length > maxLen) throw new ValidationError(`${field} too long`, field);
    return stripped;
}

/** Validate positive number. */
export function validatePositiveNumber(value: unknown, field: string): number {
    const num = Number(value);
    if (isNaN(num)) throw new ValidationError(`${field} must be a number`, field);
    if (num <= 0) throw new ValidationError(`${field} must be positive`, field);
    return num;
}

/** Validate enum value. */
export function validateEnum(value: string, allowed: string[], field: string): string {
    if (!allowed.includes(value)) {
        throw new ValidationError(`Invalid ${field}: '${value}'. Allowed: ${allowed.join(", ")}`, field);
    }
    return value;
}
