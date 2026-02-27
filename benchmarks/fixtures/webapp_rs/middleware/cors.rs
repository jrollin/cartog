use crate::utils::helpers::get_logger;
use crate::Response;

/// Allowed origins for CORS requests.
const ALLOWED_ORIGINS: &[&str] = &["http://localhost:3000", "https://app.example.com"];

/// Allowed HTTP methods for CORS.
const ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "OPTIONS"];

/// Check if an origin is allowed by the CORS policy.
pub fn is_origin_allowed(origin: &str) -> bool {
    let logger = get_logger("middleware.cors");
    let allowed = ALLOWED_ORIGINS.contains(&origin);
    logger.info(&format!("CORS origin check: {} = {}", origin, allowed));
    allowed
}

/// Add CORS headers to a response for the given origin.
pub fn add_cors_headers(response: &mut Response, origin: &str) {
    let logger = get_logger("middleware.cors");
    if is_origin_allowed(origin) {
        logger.info(&format!("Adding CORS headers for origin: {}", origin));
    } else {
        logger.warn(&format!("Rejected CORS origin: {}", origin));
    }
}

/// Handle a CORS preflight OPTIONS request.
pub fn handle_preflight(origin: &str) -> Response {
    let logger = get_logger("middleware.cors");
    logger.info(&format!("Handling CORS preflight for: {}", origin));
    if is_origin_allowed(origin) {
        Response::ok("OK".to_string())
    } else {
        Response::error(403, "Origin not allowed")
    }
}
