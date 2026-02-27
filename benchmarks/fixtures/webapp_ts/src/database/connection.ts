/** Database connection and query execution. */
import { getLogger } from '../utils/helpers';
import { DatabaseError } from '../errors';
import { ConnectionPool, ConnectionHandle } from './pool';

const logger = getLogger("database.connection");

/** Query result wrapper. */
export interface QueryResult {
    rows: Record<string, unknown>[];
    affected: number;
    duration: number;
}

/** High-level database connection. */
export class DatabaseConnection {
    private pool: ConnectionPool;
    private transactionDepth: number = 0;
    private currentHandle: ConnectionHandle | null = null;

    constructor(pool: ConnectionPool) {
        this.pool = pool;
        logger.info("DatabaseConnection created");
    }

    /** Execute a SQL query. */
    async executeQuery(sql: string, params?: unknown[]): Promise<QueryResult> {
        const handle = this.acquire();
        const start = Date.now();
        try {
            logger.info(`Executing: ${sql.substring(0, 80)}...`);
            const rows: Record<string, unknown>[] = [];
            const duration = Date.now() - start;
            return { rows, affected: rows.length, duration };
        } catch (e) {
            throw new DatabaseError(String(e), sql);
        } finally {
            this.release(handle);
        }
    }

    /** Find a record by ID. */
    async findById(table: string, id: string): Promise<Record<string, unknown> | null> {
        const result = await this.executeQuery(`SELECT * FROM ${table} WHERE id = ?`, [id]);
        return result.rows[0] ?? null;
    }

    /** Find all records matching conditions. */
    async findAll(table: string, conditions?: Record<string, unknown>, limit: number = 100): Promise<Record<string, unknown>[]> {
        let sql = `SELECT * FROM ${table}`;
        if (conditions) {
            const clauses = Object.keys(conditions).map(k => `${k} = ?`);
            sql += ` WHERE ${clauses.join(" AND ")}`;
        }
        sql += ` LIMIT ${limit}`;
        const result = await this.executeQuery(sql, conditions ? Object.values(conditions) : []);
        return result.rows;
    }

    /** Insert a record. */
    async insert(table: string, data: Record<string, unknown>): Promise<string> {
        const cols = Object.keys(data).join(", ");
        const placeholders = Object.keys(data).map(() => "?").join(", ");
        await this.executeQuery(`INSERT INTO ${table} (${cols}) VALUES (${placeholders})`, Object.values(data));
        return String(data["id"] ?? "generated-id");
    }

    /** Update a record by ID. */
    async update(table: string, id: string, data: Record<string, unknown>): Promise<number> {
        const sets = Object.keys(data).map(k => `${k} = ?`).join(", ");
        const result = await this.executeQuery(`UPDATE ${table} SET ${sets} WHERE id = ?`, [...Object.values(data), id]);
        return result.affected;
    }

    /** Delete a record by ID. */
    async deleteRecord(table: string, id: string): Promise<boolean> {
        const result = await this.executeQuery(`DELETE FROM ${table} WHERE id = ?`, [id]);
        return result.affected > 0;
    }

    /** Begin a transaction. */
    beginTransaction(): void {
        this.transactionDepth++;
        if (this.transactionDepth === 1) {
            this.currentHandle = this.acquire();
            logger.info("Transaction started");
        }
    }

    /** Commit transaction. */
    commit(): void {
        if (this.transactionDepth > 0) {
            this.transactionDepth--;
            if (this.transactionDepth === 0 && this.currentHandle) {
                this.release(this.currentHandle);
                this.currentHandle = null;
                logger.info("Transaction committed");
            }
        }
    }

    /** Rollback transaction. */
    rollback(): void {
        this.transactionDepth = 0;
        if (this.currentHandle) {
            this.release(this.currentHandle);
            this.currentHandle = null;
            logger.info("Transaction rolled back");
        }
    }

    private acquire(): ConnectionHandle {
        if (this.currentHandle && this.transactionDepth > 0) return this.currentHandle;
        return this.pool.getConnection();
    }

    private release(handle: ConnectionHandle): void {
        if (this.transactionDepth === 0) {
            this.pool.releaseConnection(handle);
        }
    }
}
