/** CORS middleware. */
import { getLogger, validateRequest } from '../utils/helpers';

const logger = getLogger("middleware.cors");

const DEFAULT_ORIGINS = ["http://localhost:3000", "https://app.example.com"];

/** CORS policy. */
export class CorsPolicy {
    constructor(
        public allowedOrigins: string[] = DEFAULT_ORIGINS,
        public allowedMethods: string[] = ["GET", "POST", "PUT", "DELETE"],
        public allowCredentials: boolean = true,
        public maxAge: number = 86400,
    ) {}

    isOriginAllowed(origin: string): boolean {
        return this.allowedOrigins.includes("*") || this.allowedOrigins.includes(origin);
    }

    getHeaders(origin: string): Record<string, string> {
        if (!this.isOriginAllowed(origin)) return {};
        return {
            "Access-Control-Allow-Origin": origin,
            "Access-Control-Allow-Methods": this.allowedMethods.join(", "),
            "Access-Control-Max-Age": String(this.maxAge),
        };
    }
}

/** Apply CORS. */
export function corsMiddleware(request: Record<string, unknown>, policy?: CorsPolicy): Record<string, unknown> {
    validateRequest(request);
    const cors = policy ?? new CorsPolicy();
    const origin = (request["origin"] as string) ?? "";
    if (origin) {
        const headers = cors.getHeaders(origin);
        if (!Object.keys(headers).length) logger.warn(`CORS rejected: ${origin}`);
        return { ...request, corsHeaders: headers };
    }
    return { ...request, corsHeaders: {} };
}
