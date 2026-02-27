/** Predefined query builders. */
import { getLogger } from '../utils/helpers';
import { DatabaseConnection, QueryResult } from './connection';

const logger = getLogger("database.queries");

/** User queries. */
export class UserQueries {
    constructor(private db: DatabaseConnection) {}

    async findByEmail(email: string): Promise<Record<string, unknown> | null> {
        logger.info(`Finding user by email: ${email}`);
        const result = await this.db.executeQuery("SELECT * FROM users WHERE email = ?", [email]);
        return result.rows[0] ?? null;
    }

    async findActiveUsers(limit: number = 100): Promise<Record<string, unknown>[]> {
        const result = await this.db.executeQuery("SELECT * FROM users WHERE active = 1 LIMIT ?", [limit]);
        return result.rows;
    }

    async searchUsers(query: string): Promise<QueryResult> {
        return this.db.executeQuery("SELECT * FROM users WHERE name LIKE ? OR email LIKE ?", [`%${query}%`, `%${query}%`]);
    }

    async softDelete(userId: string): Promise<boolean> {
        logger.info(`Soft-deleting user ${userId}`);
        const affected = await this.db.update("users", userId, { deletedAt: Date.now() });
        return affected > 0;
    }
}

/** Session queries. */
export class SessionQueries {
    constructor(private db: DatabaseConnection) {}

    async findActiveSession(token: string): Promise<Record<string, unknown> | null> {
        const result = await this.db.executeQuery("SELECT * FROM sessions WHERE token_hash = ?", [token]);
        return result.rows[0] ?? null;
    }

    async createSession(userId: string, tokenHash: string, ip: string): Promise<string> {
        logger.info(`Creating session for user ${userId}`);
        return this.db.insert("sessions", { userId, tokenHash, ipAddress: ip, createdAt: Date.now() });
    }

    async expireSession(sessionId: string): Promise<boolean> {
        const affected = await this.db.update("sessions", sessionId, { expiredAt: Date.now() });
        return affected > 0;
    }
}

/** Payment queries. */
export class PaymentQueries {
    constructor(private db: DatabaseConnection) {}

    async findByTransactionId(txnId: string): Promise<Record<string, unknown> | null> {
        const result = await this.db.executeQuery("SELECT * FROM payments WHERE transaction_id = ?", [txnId]);
        return result.rows[0] ?? null;
    }

    async findUserPayments(userId: string, status?: string): Promise<Record<string, unknown>[]> {
        logger.info(`Finding payments for user ${userId}`);
        if (status) {
            const result = await this.db.executeQuery("SELECT * FROM payments WHERE user_id = ? AND status = ?", [userId, status]);
            return result.rows;
        }
        const result = await this.db.executeQuery("SELECT * FROM payments WHERE user_id = ?", [userId]);
        return result.rows;
    }

    async createPayment(userId: string, amount: number, currency: string, txnId: string): Promise<string> {
        return this.db.insert("payments", { userId, amount, currency, transactionId: txnId, status: "pending", createdAt: Date.now() });
    }

    async updateStatus(txnId: string, status: string): Promise<boolean> {
        logger.info(`Updating payment ${txnId} to ${status}`);
        const result = await this.db.executeQuery("UPDATE payments SET status = ? WHERE transaction_id = ?", [status, txnId]);
        return result.affected > 0;
    }

    async calculateRevenue(startDate: string, endDate: string): Promise<number> {
        const result = await this.db.executeQuery(
            "SELECT SUM(amount) as total FROM payments WHERE status = 'completed' AND created_at BETWEEN ? AND ?",
            [startDate, endDate]
        );
        const row = result.rows[0];
        return row ? Number(row["total"] ?? 0) : 0;
    }
}
