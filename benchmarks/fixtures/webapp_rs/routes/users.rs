use crate::utils::helpers::{get_logger, sanitize_input};
use crate::auth::middleware::auth_middleware;
use crate::validators::user;
use crate::models::user::User;
use crate::Request;
use crate::Response;

/// Handle user profile retrieval.
pub fn get_profile_handler(request: Request) -> Response {
    let logger = get_logger("routes.users");
    if let Err(e) = auth_middleware(&request) {
        return Response::error(401, &format!("{}", e));
    }
    logger.info("Getting user profile");
    Response::ok("{"id": 1, "email": "user@example.com"}".to_string())
}

/// Handle user profile update.
pub fn update_profile_handler(request: Request) -> Response {
    let logger = get_logger("routes.users");
    if let Err(e) = auth_middleware(&request) {
        return Response::error(401, &format!("{}", e));
    }
    let name = sanitize_input("John Doe");
    let email = sanitize_input("john@example.com");
    match user::validate_update(&name, &email) {
        Ok(()) => {
            logger.info("Profile updated");
            Response::ok("{"status": "updated"}".to_string())
        }
        Err(errors) => Response::error(400, &format!("{:?}", errors)),
    }
}

/// Handle user registration.
pub fn register_handler(request: Request) -> Response {
    let logger = get_logger("routes.users");
    let email = sanitize_input("new@example.com");
    let name = sanitize_input("New User");
    let password = "securepassword123";
    match user::validate(&email, &name, password) {
        Ok(()) => {
            logger.info(&format!("User registered: {}", email));
            Response::ok("{"status": "registered"}".to_string())
        }
        Err(errors) => Response::error(400, &format!("{:?}", errors)),
    }
}
