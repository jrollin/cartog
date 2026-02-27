/** Service base and hierarchy. */
import { getLogger } from '../utils/helpers';
import { DatabaseConnection } from '../database/connection';

const logger = getLogger("services.base");

/** Service interface. */
export interface Service {
    initialize(): void;
    shutdown(): void;
    healthCheck(): Record<string, unknown>;
}

/** Auditable interface. */
export interface Auditable {
    recordAudit(action: string, actor: string, resource: string, details?: Record<string, unknown>): void;
    getAuditTrail(resource?: string, limit?: number): Record<string, unknown>[];
}

/** Re-exported BaseService for services layer. */
export { BaseService } from '../auth/service';
