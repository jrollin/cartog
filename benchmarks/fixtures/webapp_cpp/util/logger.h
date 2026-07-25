#pragma once

#include <string>

namespace webapp {
namespace util {

/// Simple structured logger for a named component.
class Logger {
public:
    explicit Logger(std::string name);

    /// Factory: creates a Logger for the given component name.
    static Logger get_logger(const std::string& name);

    void info(const std::string& message) const;
    void warn(const std::string& message) const;
    void error(const std::string& message) const;
    void debug(const std::string& message) const;

    const std::string& name() const;

private:
    std::string name_;
};

}  // namespace util
}  // namespace webapp
