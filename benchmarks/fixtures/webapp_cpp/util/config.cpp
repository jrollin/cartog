#include "util/config.h"

#include <string>

namespace webapp {
namespace util {

const std::string& Config::database_url() const {
    return database_url_;
}

void Config::set_database_url(const std::string& url) {
    database_url_ = url;
}

Config::Builder& Config::Builder::with_database_url(const std::string& url) {
    database_url_ = url;
    return *this;
}

Config Config::Builder::build() const {
    Config config;
    config.set_database_url(database_url_);
    return config;
}

}  // namespace util
}  // namespace webapp
