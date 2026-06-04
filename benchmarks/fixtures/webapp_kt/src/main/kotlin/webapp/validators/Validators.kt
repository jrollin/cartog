package webapp.validators

import webapp.errors.ValidationError

/** Conformance for any type that can validate itself. */
interface Validating {
    fun validate()
}

/** Validates user-supplied registration data. */
class UserValidator(val email: String) : Validating {
    override fun validate() {
        if (!email.contains("@")) {
            throw ValidationError("email", "invalid email")
        }
    }
}

/** Validates payment payloads. */
class PaymentValidator(val amount: Int) : Validating {
    override fun validate() {
        if (amount <= 0) {
            throw ValidationError("amount", "must be positive")
        }
    }
}
