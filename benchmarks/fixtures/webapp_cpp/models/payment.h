#pragma once

#include <string>

namespace webapp {
namespace models {

/// A single payment record awaiting capture.
class Payment {
public:
    Payment(double amount, std::string currency);

    /// Validate the payment's own fields, throwing ValidationError when invalid.
    void validate() const;

    double amount() const;
    const std::string& currency() const;

private:
    double amount_;
    std::string currency_;
};

}  // namespace models
}  // namespace webapp
