/** User model. */
import { UserRole } from './types';
import { getLogger } from '../utils/helpers';

const logger = getLogger("models.user");

export interface UserRecord {
    id: string;
    email: string;
    name: string;
    role: UserRole;
    passwordHash: string;
    active: boolean;
    createdAt: number;
    deletedAt: number | null;
}

export class User {
    public readonly id: string;
    public email: string;
    public name: string;
    public role: UserRole;
    public passwordHash: string;
    public active: boolean;
    public createdAt: number;
    public deletedAt: number | null;

    constructor(data: UserRecord) {
        this.id = data.id;
        this.email = data.email;
        this.name = data.name;
        this.role = data.role;
        this.passwordHash = data.passwordHash;
        this.active = data.active;
        this.createdAt = data.createdAt;
        this.deletedAt = data.deletedAt;
    }

    isAdmin(): boolean {
        return this.role === UserRole.Admin;
    }

    softDelete(): void {
        this.deletedAt = Date.now();
        this.active = false;
        logger.info(`User ${this.id} soft-deleted`);
    }

    toJSON(): Record<string, unknown> {
        return {
            id: this.id,
            email: this.email,
            name: this.name,
            role: this.role,
            active: this.active,
            createdAt: this.createdAt,
        };
    }
}
