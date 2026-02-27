/** Database migration management. */
import { getLogger } from '../utils/helpers';
import { DatabaseConnection } from './connection';
import { DatabaseError } from '../errors';

const logger = getLogger("database.migrations");

interface Migration {
    version: string;
    name: string;
    sql: string;
}

const MIGRATIONS: Migration[] = [
    { version: "001", name: "create_users", sql: "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT, name TEXT, role TEXT)" },
    { version: "002", name: "create_sessions", sql: "CREATE TABLE sessions (id TEXT PRIMARY KEY, userId TEXT, tokenHash TEXT)" },
    { version: "003", name: "create_payments", sql: "CREATE TABLE payments (id TEXT PRIMARY KEY, userId TEXT, amount REAL)" },
    { version: "004", name: "create_events", sql: "CREATE TABLE events (id TEXT PRIMARY KEY, type TEXT, payload TEXT)" },
    { version: "005", name: "create_notifications", sql: "CREATE TABLE notifications (id TEXT PRIMARY KEY, userId TEXT, channel TEXT)" },
];

/** Run pending migrations. */
export class MigrationRunner {
    constructor(private db: DatabaseConnection) {
        logger.info("MigrationRunner initialized");
    }

    async runPending(): Promise<number> {
        let count = 0;
        for (const migration of MIGRATIONS) {
            logger.info(`Applying migration ${migration.version}: ${migration.name}`);
            try {
                this.db.beginTransaction();
                await this.db.executeQuery(migration.sql);
                this.db.commit();
                count++;
            } catch (e) {
                this.db.rollback();
                throw new DatabaseError(`Migration ${migration.version} failed: ${e}`);
            }
        }
        logger.info(`${count} migrations applied`);
        return count;
    }

    async status(): Promise<{ applied: number; pending: number; total: number }> {
        return { applied: 0, pending: MIGRATIONS.length, total: MIGRATIONS.length };
    }
}
