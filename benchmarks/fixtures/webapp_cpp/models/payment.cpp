#include "models/payment.h"

#include <string>
#include <utility>

#include "errors/exceptions.h"

namespace webapp {
namespace models {

Payment::Payment(double amount, std::string currency)
    : amount_(amount), currency_(std::move(currency)) {}

/// Validate the payment's own fields, throwing ValidationError when invalid.
void Payment::validate() const {
    if (amount_ <= 0.0) {
        throw errors::ValidationError("amount must be positive");
    }
    if (currency_.empty()) {
        throw errors::ValidationError("currency is required");
    }
}

double Payment::amount() const {
    return amount_;
}

const std::string& Payment::currency() const {
    return currency_;
}

}  // namespace models
}  // namespace webapp
