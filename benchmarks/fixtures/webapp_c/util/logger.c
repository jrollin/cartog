#include <stdio.h>

#include "util/logger.h"

/// Factory: returns a Logger for the given component name.
struct Logger get_logger(const char *name)
{
    struct Logger log;
    log.name = name;
    return log;
}

void logger_info(const struct Logger *log, const char *message)
{
    printf("[INFO] [%s] %s\n", log->name, message);
}

void logger_warn(const struct Logger *log, const char *message)
{
    printf("[WARN] [%s] %s\n", log->name, message);
}

void logger_error(const struct Logger *log, const char *message)
{
    printf("[ERROR] [%s] %s\n", log->name, message);
}

void logger_debug(const struct Logger *log, const char *message)
{
    printf("[DEBUG] [%s] %s\n", log->name, message);
}
