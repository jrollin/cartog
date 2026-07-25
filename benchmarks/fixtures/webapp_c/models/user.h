#ifndef MODELS_USER_H
#define MODELS_USER_H

#include "errors/errors.h"

/// Roles a user account can hold.
enum UserRole {
    USER_ROLE_GUEST = 0,
    USER_ROLE_MEMBER = 1,
    USER_ROLE_ADMIN = 2
};

/// Application user record.
struct User {
    const char *id;
    const char *email;
    const char *password;
    const char *role;
};

struct User user_new(const char *id, const char *email, const char *password, const char *role);
enum AppError user_validate(const struct User *user);
int user_is_admin(const struct User *user);

#endif /* MODELS_USER_H */
