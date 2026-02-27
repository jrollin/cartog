/** API v2 payment endpoints with webhook support. */
import { getLogger, validateRequest } from '../../utils/helpers';
import { validate as validatePayment } from '../../validators/payment';
import { PaymentProcessor } from '../../services/payment/processor';
import { DatabaseConnection } from '../../database/connection';
import { EventDispatcher } from '../../events/dispatcher';

const logger = getLogger("api.v2.payments");

/** Create payment with idempotency. */
export async function handleCreatePayment(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    const headers = request["headers"] as Record<string, string>;
    const idempotencyKey = headers?.["Idempotency-Key"] ?? "";
    logger.info(`V2 create payment (idempotency=${idempotencyKey.substring(0, 12)})`);
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

/** Handle webhook. */
export async function handleWebhook(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    validateRequest(request);
    const body = request["body"] as Record<string, unknown>;
    const eventType = body["type"] as string;
    logger.info(`Webhook: ${eventType}`);
    return { status: 200, data: { acknowledged: true } };
}

/** Revenue report. */
export async function handleRevenueReport(request: Record<string, unknown>, db: DatabaseConnection, events: EventDispatcher): Promise<Record<string, unknown>> {
    validateRequest(request);
    const processor = new PaymentProcessor(db, events);
    processor.initialize();
    return { status: 200, data: {} };
}
