#ifndef UTIL_LOGGER_H
#define UTIL_LOGGER_H

/// Simple structured logger for a named component.
struct Logger {
    const char *name;
};

/// Factory: returns a Logger for the given component name.
struct Logger get_logger(const char *name);

void logger_info(const struct Logger *log, const char *message);
void logger_warn(const struct Logger *log, const char *message);
void logger_error(const struct Logger *log, const char *message);
void logger_debug(const struct Logger *log, const char *message);

#endif /* UTIL_LOGGER_H */
