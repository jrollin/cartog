#include <stddef.h>
#include <string.h>

#include "validators/user_validator.h"

#include "errors/errors.h"
#include "util/logger.h"

/// File-local `validate` (see models/user.c for why these are `static`).
static enum AppError validate(const char *email)
{
    if (email == NULL || email[0] == '\0') {
        struct ValidationError err = validation_error_new("email", "email is required");
        return err.base.code;
    }
    if (strchr(email, '@') == NULL) {
        struct ValidationError err = validation_error_new("email", "email must contain @");
        return err.base.code;
    }
    return APP_OK;
}

struct UserValidator user_validator_new(void)
{
    struct UserValidator validator;
    validator.min_password_length = 6;
    return validator;
}

enum AppError user_validator_validate(struct UserValidator *validator, const char *email)
{
    struct Logger log = get_logger("validators.user");
    logger_info(&log, "Validating user input");
    (void)validator;
    return validate(email);
}

enum AppError user_validator_validate_login(struct UserValidator *validator, const char *email,
                                            const char *password)
{
    enum AppError err = validate(email);
    if (err != APP_OK) {
        return err;
    }
    if (password == NULL || (int)strlen(password) < validator->min_password_length) {
        struct ValidationError too_short =
            validation_error_new("password", "password too short");
        return too_short.base.code;
    }
    return APP_OK;
}
