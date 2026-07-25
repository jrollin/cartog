#ifndef MODELS_PAYMENT_H
#define MODELS_PAYMENT_H

#include "errors/errors.h"

/// A payment charged against a user account.
struct Payment {
    const char *id;
    double amount;
    const char *currency;
};

struct Payment payment_new(const char *id, double amount, const char *currency);
enum AppError payment_validate(const struct Payment *payment);

#endif /* MODELS_PAYMENT_H */
