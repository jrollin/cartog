#pragma once

#include <string>

#include "util/logger.h"

namespace webapp {
namespace validators {

/// Validates payment input before it reaches a gateway.
class PaymentValidator {
public:
    PaymentValidator();

    /// Reject a non-positive amount.
    void validate(double amount) const;

    /// Reject a currency code that is not three letters.
    void validate_currency(const std::string& currency) const;

private:
    util::Logger log_;
};

}  // namespace validators
}  // namespace webapp
