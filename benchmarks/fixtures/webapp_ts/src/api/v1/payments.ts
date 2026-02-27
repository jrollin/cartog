/** API v1 payment endpoints. */
import { getLogger, validateRequest } from '../../utils/helpers';
import { validate as validatePayment } from '../../validators/payment';
import { PaymentProcessor } from '../../services/payment/processor';
import { DatabaseConnection } from '../../database/connection';
import { EventDispatcher } from '../../events/dispatcher';

const logger = getLogger("api.v1.payments");

/** Create payment. */
export async function handleCreatePayment(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    const paymentData = validatePayment(body);
    const processor = new PaymentProcessor(db, events);
    processor.initialize();
    const result = await processor.processPayment(
        paymentData["user_id"] as string,
        paymentData["amount"] as number,
        paymentData["currency"] as string,
    );
    return { status: 201, data: result };
}

/** Refund payment. */
export async function handleRefund(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    const processor = new PaymentProcessor(db, events);
    processor.initialize();
    const result = await processor.refund(body["transaction_id"] as string, (body["reason"] as string) ?? "");
    return { status: 200, data: result };
}
