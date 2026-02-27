use crate::utils::helpers::get_logger;
use crate::validators::common::{validate_not_empty, validate_range};

/// Supported payment currencies.
const SUPPORTED_CURRENCIES: &[&str] = &["USD", "EUR", "GBP", "JPY", "CAD"];

/// Validate payment request parameters.
pub fn validate(amount: f64, currency: &str, source: &str) -> Result<(), Vec<String>> {
    let logger = get_logger("validators.payment");
    logger.info(&format!("Validating payment: {} {} from {}", amount, currency, source));
    let mut errors = Vec::new();
    if let Err(e) = validate_range("amount", amount, 0.01, 999999.0) {
        errors.push(e);
    }
    if !SUPPORTED_CURRENCIES.contains(&currency) {
        errors.push(format!("Unsupported currency: {}", currency));
    }
    if let Err(e) = validate_not_empty("source", source) {
        errors.push(e);
    }
    if errors.is_empty() {
        logger.info("Payment validation passed");
        Ok(())
    } else {
        logger.warn(&format!("Payment validation failed: {} errors", errors.len()));
        Err(errors)
    }
}

/// Validate refund request parameters.
pub fn validate_refund(transaction_id: &str, reason: &str) -> Result<(), Vec<String>> {
    let logger = get_logger("validators.payment");
    logger.info(&format!("Validating refund for txn: {}", transaction_id));
    let mut errors = Vec::new();
    if let Err(e) = validate_not_empty("transaction_id", transaction_id) {
        errors.push(e);
    }
    if let Err(e) = validate_not_empty("reason", reason) {
        errors.push(e);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
