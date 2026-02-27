/** Payment input validation. */
import { getLogger } from '../utils/helpers';
import { ValidationError } from '../errors';
import { validatePositiveNumber, validateEnum } from './common';

const logger = getLogger("validators.payment");

const CURRENCIES = ["USD", "EUR", "GBP", "JPY", "CAD"];
const METHODS = ["card", "bank_transfer", "wallet"];

/** Validate payment data — name collision with validators/user, api/v1/auth, api/v2/auth. */
export function validate(data: Record<string, unknown>): Record<string, unknown> {
    logger.info("Validating payment data");
    if (!data["amount"]) throw new ValidationError("Amount required", "amount");
    if (!data["currency"]) throw new ValidationError("Currency required", "currency");
    if (!data["user_id"]) throw new ValidationError("User ID required", "user_id");
    const result: Record<string, unknown> = {};
    result["amount"] = validatePositiveNumber(data["amount"], "amount");
    result["currency"] = validateEnum(data["currency"] as string, CURRENCIES, "currency");
    result["user_id"] = data["user_id"];
    result["payment_method"] = data["payment_method"] ? validateEnum(data["payment_method"] as string, METHODS, "payment_method") : "card";
    return result;
}

/** Validate refund data. */
export function validateRefund(data: Record<string, unknown>): Record<string, unknown> {
    if (!data["transaction_id"]) throw new ValidationError("Transaction ID required", "transaction_id");
    const result: Record<string, unknown> = { transaction_id: data["transaction_id"] };
    if (data["amount"]) result["amount"] = validatePositiveNumber(data["amount"], "amount");
    if (data["reason"]) result["reason"] = String(data["reason"]).substring(0, 500);
    return result;
}
