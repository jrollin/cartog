/** Payment routes. */
import { getLogger, validateRequest } from '../utils/helpers';
import { PaymentProcessor } from '../services/payment/processor';
import { DatabaseConnection } from '../database/connection';
import { EventDispatcher } from '../events/dispatcher';
import { extractToken } from '../auth/tokens';

const logger = getLogger("routes.payments");

/** Create payment route. */
export async function createPaymentRoute(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    const processor = new PaymentProcessor(db, events);
    processor.initialize();
    const result = await processor.processPayment(
        body["user_id"] as string,
        Number(body["amount"]),
        (body["currency"] as string) ?? "USD",
    );
    return { status: 201, data: result };
}

/** Refund route. */
export async function refundRoute(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    const processor = new PaymentProcessor(db, events);
    processor.initialize();
    const result = await processor.refund(body["transaction_id"] as string);
    return { status: 200, data: result };
}
