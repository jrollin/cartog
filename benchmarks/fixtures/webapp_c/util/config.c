#include "util/config.h"

#include "util/logger.h"

/// Start a builder pre-seeded with the defaults.
struct ConfigBuilder config_builder_new(void)
{
    struct ConfigBuilder builder;
    builder.config = default_config();
    return builder;
}

void config_builder_with_database_url(struct ConfigBuilder *builder, const char *url)
{
    builder->config.database_url = url;
}

struct Config config_builder_build(const struct ConfigBuilder *builder)
{
    struct Logger log = get_logger("util.config");
    logger_debug(&log, "Building config");
    return builder->config;
}

/// The compiled-in defaults used when nothing overrides them.
struct Config default_config(void)
{
    struct Config config;
    config.database_url = "postgres://localhost:5432/app";
    config.port = 8080;
    return config;
}
