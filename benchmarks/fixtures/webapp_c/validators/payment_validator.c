#include "validators/payment_validator.h"

#include "errors/errors.h"
#include "models/payment.h"
#include "util/logger.h"

/// File-local `validate` (see models/user.c for why these are `static`).
static enum AppError validate(const struct Payment *payment, double max_amount)
{
    if (payment->amount <= 0.0) {
        struct ValidationError err = validation_error_new("amount", "amount must be positive");
        return err.base.code;
    }
    if (payment->amount > max_amount) {
        struct ValidationError err = validation_error_new("amount", "amount exceeds limit");
        return err.base.code;
    }
    return APP_OK;
}

struct PaymentValidator payment_validator_new(double max_amount)
{
    struct PaymentValidator validator;
    validator.max_amount = max_amount;
    return validator;
}

enum AppError payment_validator_validate(struct PaymentValidator *validator,
                                         const struct Payment *payment)
{
    struct Logger log = get_logger("validators.payment");
    logger_info(&log, "Validating payment");
    return validate(payment, validator->max_amount);
}
