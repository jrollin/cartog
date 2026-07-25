#include "util/logger.h"

#include <iostream>
#include <string>
#include <utility>

namespace webapp {
namespace util {

Logger::Logger(std::string name) : name_(std::move(name)) {}

/// Factory: creates a Logger for the given component name.
Logger Logger::get_logger(const std::string& name) {
    return Logger(name);
}

void Logger::info(const std::string& message) const {
    std::cout << "[INFO] [" << name_ << "] " << message << std::endl;
}

void Logger::warn(const std::string& message) const {
    std::cout << "[WARN] [" << name_ << "] " << message << std::endl;
}

void Logger::error(const std::string& message) const {
    std::cout << "[ERROR] [" << name_ << "] " << message << std::endl;
}

void Logger::debug(const std::string& message) const {
    std::cout << "[DEBUG] [" << name_ << "] " << message << std::endl;
}

const std::string& Logger::name() const {
    return name_;
}

}  // namespace util
}  // namespace webapp
