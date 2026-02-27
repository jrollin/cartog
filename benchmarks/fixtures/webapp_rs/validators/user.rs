use crate::utils::helpers::get_logger;
use crate::validators::common::{
    validate_email_format, validate_max_length, validate_min_length, validate_not_empty,
};

/// Validate user registration input fields.
pub fn validate(email: &str, name: &str, password: &str) -> Result<(), Vec<String>> {
    let logger = get_logger("validators.user");
    logger.info(&format!("Validating user registration: {}", email));
    let mut errors = Vec::new();
    if let Err(e) = validate_not_empty("email", email) {
        errors.push(e);
    }
    if let Err(e) = validate_email_format(email) {
        errors.push(e);
    }
    if let Err(e) = validate_not_empty("name", name) {
        errors.push(e);
    }
    if let Err(e) = validate_max_length("name", name, 100) {
        errors.push(e);
    }
    if let Err(e) = validate_min_length("password", password, 8) {
        errors.push(e);
    }
    if let Err(e) = validate_max_length("password", password, 128) {
        errors.push(e);
    }
    if errors.is_empty() {
        logger.info("User validation passed");
        Ok(())
    } else {
        logger.warn(&format!("User validation failed: {} errors", errors.len()));
        Err(errors)
    }
}

/// Validate user profile update fields.
pub fn validate_update(name: &str, email: &str) -> Result<(), Vec<String>> {
    let logger = get_logger("validators.user");
    logger.info("Validating user update");
    let mut errors = Vec::new();
    if let Err(e) = validate_not_empty("name", name) {
        errors.push(e);
    }
    if let Err(e) = validate_email_format(email) {
        errors.push(e);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
