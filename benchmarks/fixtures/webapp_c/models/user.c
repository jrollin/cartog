#include <string.h>

#include "models/user.h"

#include "errors/errors.h"
#include "util/logger.h"

/// One of four file-local `validate` functions in the fixture; `static` gives
/// each one internal linkage so the same name can be defined in every file.
static enum AppError validate(const struct User *user)
{
    if (user->email == NULL || user->email[0] == '\0') {
        struct ValidationError err = validation_error_new("email", "email is required");
        return err.base.code;
    }
    return APP_OK;
}

struct User user_new(const char *id, const char *email, const char *password, const char *role)
{
    struct User user;
    user.id = id;
    user.email = email;
    user.password = password;
    user.role = role;
    return user;
}

/// Public entry point over the file-local `validate`.
enum AppError user_validate(const struct User *user)
{
    struct Logger log = get_logger("models.user");
    logger_debug(&log, "Validating user model");
    return validate(user);
}

int user_is_admin(const struct User *user)
{
    return user->role != NULL && strcmp(user->role, "admin") == 0;
}
