/** High-level authentication service. */
import { getLogger, sanitizeInput } from '../utils/helpers';
import { AuthService } from '../auth/service';
import { validateToken, generateToken } from '../auth/tokens';
import { DatabaseConnection } from '../database/connection';
import { UserQueries, SessionQueries } from '../database/queries';
import { EventDispatcher } from '../events/dispatcher';
import { AuthenticationError, ValidationError } from '../errors';
import { BaseService } from '../auth/service';

const logger = getLogger("services.auth");

/** Orchestrates authentication flows. */
export class AuthenticationService extends BaseService {
    private auth: AuthService;
    private users: UserQueries;
    private sessions: SessionQueries;
    private events: EventDispatcher;

    constructor(db: DatabaseConnection, events: EventDispatcher) {
        super("authentication");
        this.auth = new AuthService();
        this.users = new UserQueries(db);
        this.sessions = new SessionQueries(db);
        this.events = events;
    }

    /** Authenticate a user — main entry point for login flow. */
    async authenticate(email: string, password: string, ip: string = "unknown"): Promise<Record<string, unknown>> {
        this.requireInitialized();
        logger.info(`Authentication attempt for ${email}`);
        const cleanEmail = sanitizeInput(email);
        if (!cleanEmail) {
            throw new ValidationError("Email is required", "email");
        }
        try {
            const token = await this.auth.login(cleanEmail, password);
            if (!token) {
                this.events.emit("auth.login_failed", { email: cleanEmail, ip });
                throw new AuthenticationError("Invalid credentials");
            }
            this.events.emit("auth.login_success", { email: cleanEmail, ip });
            return { token, email: cleanEmail };
        } catch (e) {
            if (e instanceof AuthenticationError) throw e;
            this.events.emit("auth.login_failed", { email: cleanEmail, ip });
            throw new AuthenticationError(`Authentication failed: ${e}`);
        }
    }

    /** Verify a token. */
    async verifyToken(token: string): Promise<Record<string, unknown> | null> {
        try {
            const claims = validateToken(token);
            return await this.users.findByEmail(claims.email);
        } catch {
            return null;
        }
    }

    /** Log out. */
    async logout(token: string): Promise<boolean> {
        logger.info("Processing logout");
        const session = await this.sessions.findActiveSession(token);
        if (session) {
            await this.sessions.expireSession(session["id"] as string);
            this.events.emit("auth.logout", { sessionId: session["id"] });
            return true;
        }
        return false;
    }
}
