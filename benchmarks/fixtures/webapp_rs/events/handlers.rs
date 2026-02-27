use crate::utils::helpers::get_logger;
use crate::events::dispatcher::Event;

/// Handle a user registration event.
pub fn on_user_registered(event: &Event) {
    let logger = get_logger("events.handlers");
    logger.info(&format!("User registered: {}", event.payload));
}

/// Handle a successful login event.
pub fn on_login_success(event: &Event) {
    let logger = get_logger("events.handlers");
    logger.info(&format!("Login success: {}", event.payload));
}

/// Handle a failed login event.
pub fn on_login_failed(event: &Event) {
    let logger = get_logger("events.handlers");
    logger.warn(&format!("Login failed: {}", event.payload));
}

/// Handle a payment completed event.
pub fn on_payment_completed(event: &Event) {
    let logger = get_logger("events.handlers");
    logger.info(&format!("Payment completed: {}", event.payload));
}

/// Handle a payment refunded event.
pub fn on_payment_refunded(event: &Event) {
    let logger = get_logger("events.handlers");
    logger.info(&format!("Payment refunded: {}", event.payload));
}

/// Handle a password changed event.
pub fn on_password_changed(event: &Event) {
    let logger = get_logger("events.handlers");
    logger.info(&format!("Password changed: {}", event.payload));
}

/// Handle a session expired event.
pub fn on_session_expired(event: &Event) {
    let logger = get_logger("events.handlers");
    logger.info(&format!("Session expired: {}", event.payload));
}
