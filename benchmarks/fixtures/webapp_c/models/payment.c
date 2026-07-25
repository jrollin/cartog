#include <stddef.h>

#include "models/payment.h"

#include "errors/errors.h"
#include "util/logger.h"

/// File-local `validate` (see models/user.c for why these are `static`).
static enum AppError validate(const struct Payment *payment)
{
    if (payment->amount <= 0.0) {
        struct ValidationError err = validation_error_new("amount", "amount must be positive");
        return err.base.code;
    }
    if (payment->currency == NULL) {
        struct ValidationError err = validation_error_new("currency", "currency is required");
        return err.base.code;
    }
    return APP_OK;
}

struct Payment payment_new(const char *id, double amount, const char *currency)
{
    struct Payment payment;
    payment.id = id;
    payment.amount = amount;
    payment.currency = currency;
    return payment;
}

enum AppError payment_validate(const struct Payment *payment)
{
    struct Logger log = get_logger("models.payment");
    logger_debug(&log, "Validating payment model");
    return validate(payment);
}
