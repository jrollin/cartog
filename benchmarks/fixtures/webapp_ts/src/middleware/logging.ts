/** Request logging middleware. */
import { getLogger, validateRequest, generateRequestId, maskSensitive } from '../utils/helpers';

const logger = getLogger("middleware.logging");

const SENSITIVE_FIELDS = ["password", "token", "secret", "apiKey"];

/** Log incoming requests. */
export function loggingMiddleware(request: Record<string, unknown>): Record<string, unknown> {
    validateRequest(request);
    const requestId = (request["requestId"] as string) ?? generateRequestId();
    const safe = maskSensitive(request, SENSITIVE_FIELDS);
    const method = request["method"] as string;
    const path = request["path"] as string;
    logger.info(`[${requestId}] ${method} ${path}`);
    return { ...request, requestId, startTime: Date.now() };
}

/** Log response. */
export function logResponse(request: Record<string, unknown>, status: number): void {
    const requestId = request["requestId"] as string;
    const start = (request["startTime"] as number) ?? Date.now();
    const duration = Date.now() - start;
    logger.info(`[${requestId}] -> ${status} (${duration}ms)`);
}
