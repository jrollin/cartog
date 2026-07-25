#ifndef VALIDATORS_PAYMENT_VALIDATOR_H
#define VALIDATORS_PAYMENT_VALIDATOR_H

#include "errors/errors.h"
#include "models/payment.h"

/// Validates payment input.
struct PaymentValidator {
    double max_amount;
};

struct PaymentValidator payment_validator_new(double max_amount);
enum AppError payment_validator_validate(struct PaymentValidator *validator,
                                         const struct Payment *payment);

#endif /* VALIDATORS_PAYMENT_VALIDATOR_H */
