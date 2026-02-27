use crate::utils::helpers::{get_logger, sanitize_input};
use crate::auth::middleware::auth_middleware;
use crate::Request;
use crate::Response;

/// Handle listing notifications for the authenticated user.
pub fn list_notifications_handler(request: Request) -> Response {
    let logger = get_logger("routes.notifications");
    if let Err(e) = auth_middleware(&request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("Listing notifications");
    Response::ok("[]".to_string())
}

/// Handle marking a notification as read.
pub fn mark_read_handler(request: Request) -> Response {
    let logger = get_logger("routes.notifications");
    if let Err(e) = auth_middleware(&request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("Marking notification as read");
    Response::ok("{"status": "read"}".to_string())
}

/// Handle sending a test notification.
pub fn send_test_handler(request: Request) -> Response {
    let logger = get_logger("routes.notifications");
    if let Err(e) = auth_middleware(&request) {
        return Response::error(401, &format!("{}", e));
    }
    let subject = sanitize_input("Test Notification");
    let body = sanitize_input("This is a test notification body");
    logger.info(&format!("Sending test notification: {}", subject));
    Response::ok("{"status": "sent"}".to_string())
}
