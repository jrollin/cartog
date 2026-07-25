#include "validators/payment_validator.h"

#include <string>

#include "errors/exceptions.h"
#include "util/logger.h"

namespace webapp {
namespace validators {

PaymentValidator::PaymentValidator() : log_(util::Logger::get_logger("validators.payment")) {}

/// Reject a non-positive amount.
void PaymentValidator::validate(double amount) const {
    log_.info("Validating payment");
    if (amount <= 0.0) {
        throw errors::ValidationError("amount must be positive");
    }
}

/// Reject a currency code that is not three letters.
void PaymentValidator::validate_currency(const std::string& currency) const {
    if (currency.size() != 3) {
        throw errors::ValidationError("currency must be a 3-letter code");
    }
}

}  // namespace validators
}  // namespace webapp
