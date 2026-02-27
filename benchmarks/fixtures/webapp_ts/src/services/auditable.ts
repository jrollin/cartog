/** Auditable service with audit trail. */
import { getLogger } from '../utils/helpers';
import { BaseService } from '../auth/service';
import { Auditable } from './base';

const logger = getLogger("services.auditable");

interface AuditEntry {
    [key: string]: unknown;
    action: string;
    actor: string;
    resource: string;
    details: Record<string, unknown>;
    timestamp: number;
}

/** Service with audit logging. */
export class AuditableService extends BaseService implements Auditable {
    private auditLog: AuditEntry[] = [];

    constructor(serviceName: string = "auditable") {
        super(serviceName);
    }

    /** Record an audit entry. */
    recordAudit(action: string, actor: string, resource: string, details?: Record<string, unknown>): void {
        const entry: AuditEntry = {
            action, actor, resource,
            details: details ?? {},
            timestamp: Date.now(),
        };
        this.auditLog.push(entry);
        logger.info(`Audit: ${actor} ${action} on ${resource}`);
    }

    /** Query audit trail. */
    getAuditTrail(resource?: string, limit: number = 50): Record<string, unknown>[] {
        let entries = this.auditLog;
        if (resource) {
            entries = entries.filter(e => e.resource === resource);
        }
        return entries.slice(-limit).reverse();
    }
}
