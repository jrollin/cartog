/** Database connection pool. */
import { getLogger } from '../utils/helpers';
import { DatabaseError } from '../errors';

const logger = getLogger("database.pool");

export interface ConnectionHandle {
    id: string;
    createdAt: number;
    lastUsed: number;
    inUse: boolean;
    queryCount: number;
}

/** Manages a pool of database connections. */
export class ConnectionPool {
    private connections: ConnectionHandle[] = [];
    private poolSize: number;
    private initialized: boolean = false;

    constructor(private dsn: string, poolSize: number = 10) {
        this.poolSize = Math.min(poolSize, 50);
        logger.info(`Pool created: size=${this.poolSize}`);
    }

    /** Initialize the pool with connections. */
    initialize(): void {
        if (this.initialized) return;
        for (let i = 0; i < this.poolSize; i++) {
            this.connections.push({
                id: `conn-${i}`,
                createdAt: Date.now(),
                lastUsed: Date.now(),
                inUse: false,
                queryCount: 0,
            });
        }
        this.initialized = true;
        logger.info(`Pool initialized with ${this.poolSize} connections`);
    }

    /** Acquire a connection from the pool. */
    getConnection(): ConnectionHandle {
        if (!this.initialized) this.initialize();
        for (const conn of this.connections) {
            if (!conn.inUse) {
                conn.inUse = true;
                conn.lastUsed = Date.now();
                conn.queryCount++;
                logger.info(`Acquired connection ${conn.id}`);
                return conn;
            }
        }
        throw new DatabaseError("Connection pool exhausted");
    }

    /** Release a connection back to the pool. */
    releaseConnection(handle: ConnectionHandle): void {
        handle.inUse = false;
        handle.lastUsed = Date.now();
        logger.info(`Released connection ${handle.id}`);
    }

    /** Get pool statistics. */
    stats(): { total: number; active: number; idle: number } {
        const active = this.connections.filter(c => c.inUse).length;
        return { total: this.connections.length, active, idle: this.connections.length - active };
    }

    /** Shut down the pool. */
    shutdown(): void {
        this.connections = [];
        this.initialized = false;
        logger.info("Pool shut down");
    }
}
