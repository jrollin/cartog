/** Session model. */
import { getLogger } from '../utils/helpers';

const logger = getLogger("models.session");

export interface SessionRecord {
    id: string;
    userId: string;
    tokenHash: string;
    ipAddress: string;
    createdAt: number;
    expiredAt: number | null;
}

export class Session {
    public readonly id: string;
    public readonly userId: string;
    public readonly tokenHash: string;
    public readonly ipAddress: string;
    public readonly createdAt: number;
    public expiredAt: number | null;

    constructor(data: SessionRecord) {
        this.id = data.id;
        this.userId = data.userId;
        this.tokenHash = data.tokenHash;
        this.ipAddress = data.ipAddress;
        this.createdAt = data.createdAt;
        this.expiredAt = data.expiredAt;
    }

    isActive(): boolean {
        return this.expiredAt === null;
    }

    expire(): void {
        this.expiredAt = Date.now();
        logger.info(`Session ${this.id} expired`);
    }

    static create(userId: string, tokenHash: string, ip: string): Session {
        return new Session({
            id: `sess-${Date.now()}`,
            userId,
            tokenHash,
            ipAddress: ip,
            createdAt: Date.now(),
            expiredAt: null,
        });
    }
}
