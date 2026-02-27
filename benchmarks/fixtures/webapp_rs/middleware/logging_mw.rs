use crate::utils::helpers::{get_logger, generate_request_id};
use crate::Request;
use crate::Response;

/// Log an incoming request with method, path, and generated request ID.
pub fn log_request(request: &Request) -> String {
    let logger = get_logger("middleware.logging");
    let request_id = generate_request_id();
    logger.info(&format!(
        "[{}] {} {}",
        request_id,
        "GET",
        request.path,
    ));
    request_id
}

/// Log an outgoing response with status and request ID.
pub fn log_response(request_id: &str, response: &Response) {
    let logger = get_logger("middleware.logging");
    logger.info(&format!(
        "[{}] Response: status={}",
        request_id,
        response.status,
    ));
}

/// Log an error that occurred during request processing.
pub fn log_error(request_id: &str, error: &str) {
    let logger = get_logger("middleware.logging");
    logger.error(&format!("[{}] Error: {}", request_id, error));
}

/// Middleware that wraps a handler with request/response logging.
pub fn with_logging(
    request: &Request,
    handler: fn(&Request) -> Response,
) -> Response {
    let logger = get_logger("middleware.logging");
    let request_id = log_request(request);
    logger.info(&format!("[{}] Processing request", request_id));
    let response = handler(request);
    log_response(&request_id, &response);
    response
}
