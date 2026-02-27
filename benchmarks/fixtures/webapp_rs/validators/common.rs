use crate::utils::helpers::get_logger;

/// Validate that a string is not empty.
pub fn validate_not_empty(field: &str, value: &str) -> Result<(), String> {
    let logger = get_logger("validators.common");
    if value.trim().is_empty() {
        logger.warn(&format!("Validation failed: {} is empty", field));
        return Err(format!("{} cannot be empty", field));
    }
    Ok(())
}

/// Validate that a string does not exceed the maximum length.
pub fn validate_max_length(field: &str, value: &str, max: usize) -> Result<(), String> {
    let logger = get_logger("validators.common");
    if value.len() > max {
        logger.warn(&format!("Validation failed: {} exceeds max length {}", field, max));
        return Err(format!("{} exceeds maximum length of {}", field, max));
    }
    Ok(())
}

/// Validate that a string has at least the minimum length.
pub fn validate_min_length(field: &str, value: &str, min: usize) -> Result<(), String> {
    let logger = get_logger("validators.common");
    if value.len() < min {
        logger.warn(&format!("Validation failed: {} below min length {}", field, min));
        return Err(format!("{} must be at least {} characters", field, min));
    }
    Ok(())
}

/// Validate that a value is a valid email format (simple check).
pub fn validate_email_format(email: &str) -> Result<(), String> {
    let logger = get_logger("validators.common");
    if !email.contains('@') || !email.contains('.') {
        logger.warn(&format!("Invalid email format: {}", email));
        return Err("Invalid email format".to_string());
    }
    Ok(())
}

/// Validate that a numeric value is within the given range.
pub fn validate_range(field: &str, value: f64, min: f64, max: f64) -> Result<(), String> {
    let logger = get_logger("validators.common");
    if value < min || value > max {
        logger.warn(&format!("Validation failed: {} out of range [{}, {}]", field, min, max));
        return Err(format!("{} must be between {} and {}", field, min, max));
    }
    Ok(())
}
