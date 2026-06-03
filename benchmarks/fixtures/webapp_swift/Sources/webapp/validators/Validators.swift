import Foundation

/// Conformance for any type that can validate itself.
protocol Validating {
    func validate() throws
}

/// Validates user-supplied registration data.
struct UserValidator: Validating {
    let email: String

    func validate() throws {
        if !email.contains("@") {
            throw ValidationError(field: "email", message: "invalid email")
        }
    }
}

/// Validates payment payloads.
struct PaymentValidator: Validating {
    let amount: Int

    func validate() throws {
        if amount <= 0 {
            throw ValidationError(field: "amount", message: "must be positive")
        }
    }
}
