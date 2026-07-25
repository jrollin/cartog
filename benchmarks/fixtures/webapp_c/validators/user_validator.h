#ifndef VALIDATORS_USER_VALIDATOR_H
#define VALIDATORS_USER_VALIDATOR_H

#include "errors/errors.h"

/// Validates user-facing input.
struct UserValidator {
    int min_password_length;
};

struct UserValidator user_validator_new(void);
enum AppError user_validator_validate(struct UserValidator *validator, const char *email);
enum AppError user_validator_validate_login(struct UserValidator *validator, const char *email,
                                            const char *password);

#endif /* VALIDATORS_USER_VALIDATOR_H */
