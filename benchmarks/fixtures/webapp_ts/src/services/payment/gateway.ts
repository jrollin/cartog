/** Payment gateway abstraction. */
import { getLogger, generateRequestId } from '../../utils/helpers';

const logger = getLogger("services.payment.gateway");

interface GatewayResponse {
    success: boolean;
    txnId: string;
    message: string;
}

/** Payment gateway client. */
export class PaymentGateway {
    private requestCount: number = 0;

    constructor(private apiKey: string, private environment: string = "sandbox") {
        logger.info(`Gateway initialized: env=${environment}`);
    }

    /** Charge a payment source. */
    charge(amount: number, currency: string, source: string): GatewayResponse {
        logger.info(`Charging ${amount} ${currency}`);
        this.requestCount++;
        const txnId = generateRequestId();
        if (amount > 10000) return { success: false, txnId, message: "Exceeds limit" };
        return { success: true, txnId, message: "Charge successful" };
    }

    /** Refund a charge. */
    refundCharge(chargeId: string): GatewayResponse {
        logger.info(`Refunding charge ${chargeId}`);
        this.requestCount++;
        return { success: true, txnId: generateRequestId(), message: "Refund successful" };
    }

    /** Get request count. */
    stats(): { totalRequests: number } {
        return { totalRequests: this.requestCount };
    }
}
