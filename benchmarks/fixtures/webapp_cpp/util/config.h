#pragma once

#include <string>

namespace webapp {
namespace util {

/// Application configuration with a nested fluent builder.
class Config {
public:
    /// Fluent builder for Config (nested type -> dotted qualified name).
    class Builder {
    public:
        Builder& with_database_url(const std::string& url);
        Config build() const;

    private:
        std::string database_url_;
    };

    const std::string& database_url() const;
    void set_database_url(const std::string& url);

private:
    std::string database_url_;
};

}  // namespace util
}  // namespace webapp
