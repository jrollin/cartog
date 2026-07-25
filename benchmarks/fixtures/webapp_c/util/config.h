#ifndef UTIL_CONFIG_H
#define UTIL_CONFIG_H

/// Application configuration loaded once at startup.
struct Config {
    const char *database_url;
    int port;
};

/// Incremental builder for a Config value.
struct ConfigBuilder {
    struct Config config;
};

struct ConfigBuilder config_builder_new(void);
void config_builder_with_database_url(struct ConfigBuilder *builder, const char *url);
struct Config config_builder_build(const struct ConfigBuilder *builder);
struct Config default_config(void);

#endif /* UTIL_CONFIG_H */
