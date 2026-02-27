/** Payment background tasks. */
import { getLogger } from '../utils/helpers';
import { DatabaseConnection } from '../database/connection';
import { PaymentQueries } from '../database/queries';
import { PaymentProcessor } from '../services/payment/processor';
import { EventDispatcher } from '../events/dispatcher';

const logger = getLogger("tasks.payment");

/** Process pending payments. */
export async function processPendingPayments(db: DatabaseConnection, events: EventDispatcher): Promise<{ processed: number; failed: number }> {
    logger.info("Processing pending payments");
    const queries = new PaymentQueries(db);
    const pending = await queries.findUserPayments("", "pending");
    let processed = 0;
    let failed = 0;
    for (const payment of pending) {
        try {
            await queries.updateStatus(payment["transactionId"] as string, "completed");
            processed++;
        } catch {
            await queries.updateStatus(payment["transactionId"] as string, "failed");
            failed++;
        }
    }
    return { processed, failed };
}

/** Reconcile payments. */
export async function reconcilePayments(db: DatabaseConnection, events: EventDispatcher): Promise<{ resolved: number }> {
    logger.info("Reconciling payments");
    const queries = new PaymentQueries(db);
    const processing = await queries.findUserPayments("", "processing");
    let resolved = 0;
    for (const payment of processing) {
        await queries.updateStatus(payment["transactionId"] as string, "completed");
        resolved++;
    }
    return { resolved };
}
