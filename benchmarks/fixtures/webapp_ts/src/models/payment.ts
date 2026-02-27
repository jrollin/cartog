/** Payment model. */
import { PaymentStatus } from './types';
import { getLogger } from '../utils/helpers';

const logger = getLogger("models.payment");

export interface PaymentRecord {
    id: string;
    userId: string;
    amount: number;
    currency: string;
    transactionId: string;
    status: PaymentStatus;
    createdAt: number;
    completedAt: number | null;
}

export class Payment {
    public readonly id: string;
    public readonly userId: string;
    public readonly amount: number;
    public readonly currency: string;
    public readonly transactionId: string;
    public status: PaymentStatus;
    public readonly createdAt: number;
    public completedAt: number | null;

    constructor(data: PaymentRecord) {
        this.id = data.id;
        this.userId = data.userId;
        this.amount = data.amount;
        this.currency = data.currency;
        this.transactionId = data.transactionId;
        this.status = data.status;
        this.createdAt = data.createdAt;
        this.completedAt = data.completedAt;
    }

    complete(): void {
        this.status = PaymentStatus.Completed;
        this.completedAt = Date.now();
        logger.info(`Payment ${this.transactionId} completed`);
    }

    fail(reason: string): void {
        this.status = PaymentStatus.Failed;
        logger.info(`Payment ${this.transactionId} failed: ${reason}`);
    }

    refund(): void {
        this.status = PaymentStatus.Refunded;
        logger.info(`Payment ${this.transactionId} refunded`);
    }

    isCompleted(): boolean {
        return this.status === PaymentStatus.Completed;
    }

    toJSON(): Record<string, unknown> {
        return {
            id: this.id,
            userId: this.userId,
            amount: this.amount,
            currency: this.currency,
            transactionId: this.transactionId,
            status: this.status,
            createdAt: this.createdAt,
            completedAt: this.completedAt,
        };
    }
}
