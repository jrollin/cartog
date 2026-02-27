/** Authentication service with class hierarchy. */

import { getLogger } from '../utils/helpers';
import { validateToken, generateToken, TokenError } from './tokens';

const logger = getLogger("auth.service");

/** Base service providing shared functionality. */
export class BaseService {
    protected serviceName: string;
    protected initialized: boolean;

    constructor(serviceName: string = "base") {
        this.serviceName = serviceName;
        this.initialized = false;
    }

    /** Initialize the service. */
    initialize(): void {
        this.initialized = true;
        logger.info(`${this.serviceName} initialized`);
    }

    /** Shut down the service. */
    shutdown(): void {
        this.initialized = false;
        logger.info(`${this.serviceName} shut down`);
    }

    /** Check service health. */
    healthCheck(): { service: string; status: string } {
        return {
            service: this.serviceName,
            status: this.initialized ? "healthy" : "not_initialized",
        };
    }

    /** Ensure service is initialized. */
    protected requireInitialized(): void {
        if (!this.initialized) {
            throw new Error(`${this.serviceName} not initialized`);
        }
    }
}

/** User record interface. */
export interface User {
    id: string;
    email: string;
    name: string;
    role: string;
    passwordHash: string;
}

/** Authentication service handling login, logout, and user management. */
export class AuthService extends BaseService {
    private users: Map<string, User>;

    constructor() {
        super("auth");
        this.users = new Map();
    }

    /** Authenticate user and return token. */
    async login(email: string, password: string): Promise<string | null> {
        this.requireInitialized();
        logger.info(`Login attempt for ${email}`);
        const user = this._findUser(email);
        if (!user) {
            logger.warn(`User not found: ${email}`);
            return null;
        }
        // Verify password (simulated)
        if (user.passwordHash !== password) {
            return null;
        }
        const token = generateToken(user);
        logger.info(`Login successful for ${email}`);
        return token;
    }

    /** Log out a user by invalidating their token. */
    async logout(token: string): Promise<boolean> {
        this.requireInitialized();
        logger.info("Processing logout");
        try {
            validateToken(token);
            return true;
        } catch (e) {
            if (e instanceof TokenError) {
                logger.warn("Invalid token on logout");
            }
            return false;
        }
    }

    /** Get the current user from a token. */
    async getCurrentUser(token: string): Promise<User | null> {
        this.requireInitialized();
        try {
            const claims = validateToken(token);
            return this._findUser(claims.email) ?? null;
        } catch {
            return null;
        }
    }

    /** Change user password. */
    async changePassword(userId: string, oldPassword: string): Promise<boolean> {
        this.requireInitialized();
        logger.info(`Password change for ${userId}`);
        // Would verify old password and set new one
        return true;
    }

    /** Find a user by email. */
    _findUser(email: string): User | undefined {
        for (const user of this.users.values()) {
            if (user.email === email) {
                return user;
            }
        }
        return undefined;
    }
}

/** Admin service extending AuthService. */
export class AdminService extends AuthService {
    constructor() {
        super();
        this.serviceName = "admin";
    }

    /** Impersonate a user (admin only). */
    async impersonate(adminToken: string, targetUserId: string): Promise<string | null> {
        this.requireInitialized();
        const admin = await this.getCurrentUser(adminToken);
        if (!admin || admin.role !== "admin") {
            return null;
        }
        logger.info(`Admin impersonating user ${targetUserId}`);
        return generateToken({ id: targetUserId, email: "", role: "user" });
    }

    /** List all users (admin only). */
    async listAllUsers(adminToken: string): Promise<User[]> {
        this.requireInitialized();
        const admin = await this.getCurrentUser(adminToken);
        if (!admin || admin.role !== "admin") {
            return [];
        }
        return [];
    }
}
