/** Payment processor with diamond-like inheritance. */
import { getLogger, generateRequestId } from '../../utils/helpers';
import { CacheableService } from '../cacheable';
import { Auditable } from '../base';
import { DatabaseConnection } from '../../database/connection';
import { PaymentQueries } from '../../database/queries';
import { EventDispatcher } from '../../events/dispatcher';
import { PaymentError, ValidationError, NotFoundError } from '../../errors';

const logger = getLogger("services.payment");

const SUPPORTED_CURRENCIES = ["USD", "EUR", "GBP", "JPY", "CAD"];

/** Payment processor with caching + audit. */
export class PaymentProcessor extends CacheableService implements Auditable {
    private events: EventDispatcher;
    private queries: PaymentQueries;
    private auditLog: Array<{ action: string; actor: string; resource: string; details: Record<string, unknown>; timestamp: number }> = [];

    constructor(db: DatabaseConnection, events: EventDispatcher) {
        super("payment_processor");
        this.events = events;
        this.queries = new PaymentQueries(db);
    }

    /** Process a payment. */
    async processPayment(userId: string, amount: number, currency: string, method: string = "card"): Promise<Record<string, unknown>> {
        this.requireInitialized();
        logger.info(`Processing payment: user=${userId}, amount=${amount} ${currency}`);
        this.validatePayment(amount, currency);
        const txnId = generateRequestId();
        const cacheKey = `payment:${userId}:${amount}:${currency}`;
        if (this.cacheGet(cacheKey)) {
            throw new PaymentError("Duplicate payment", txnId);
        }
        try {
            await this.queries.createPayment(userId, amount, currency, txnId);
            await this.queries.updateStatus(txnId, "completed");
        } catch (e) {
            throw new PaymentError(`Payment failed: ${e}`, txnId);
        }
        this.cacheSet(cacheKey, txnId, 300);
        this.recordAudit("payment.processed", userId, `payment:${txnId}`, { amount, currency, method });
        this.events.emit("payment.completed", { transactionId: txnId, userId, amount, currency });
        return { transactionId: txnId, status: "completed", amount, currency };
    }

    /** Refund a payment. */
    async refund(transactionId: string, reason: string = ""): Promise<Record<string, unknown>> {
        logger.info(`Refunding: ${transactionId}`);
        const payment = await this.queries.findByTransactionId(transactionId);
        if (!payment) throw new NotFoundError("Payment", transactionId);
        await this.queries.updateStatus(transactionId, "refunded");
        this.recordAudit("payment.refunded", "system", `payment:${transactionId}`, { reason });
        this.events.emit("payment.refunded", { transactionId, reason });
        return { transactionId, status: "refunded" };
    }

    /** Record an audit entry. */
    recordAudit(action: string, actor: string, resource: string, details?: Record<string, unknown>): void {
        this.auditLog.push({ action, actor, resource, details: details ?? {}, timestamp: Date.now() });
        logger.info(`Audit: ${actor} ${action} on ${resource}`);
    }

    /** Get audit trail. */
    getAuditTrail(resource?: string, limit: number = 50): Record<string, unknown>[] {
        let entries = this.auditLog;
        if (resource) entries = entries.filter(e => e.resource === resource);
        return entries.slice(-limit);
    }

    private validatePayment(amount: number, currency: string): void {
        if (!SUPPORTED_CURRENCIES.includes(currency)) {
            throw new ValidationError(`Unsupported currency: ${currency}`, "currency");
        }
        if (amount <= 0) throw new ValidationError("Amount must be positive", "amount");
        if (amount > 999999) throw new ValidationError("Amount exceeds maximum", "amount");
    }
}
