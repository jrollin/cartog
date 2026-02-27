use crate::utils::helpers::get_logger;
use crate::auth::middleware::{auth_middleware, extract_token};
use crate::error::AppError;
use crate::Request;

/// Paths that do not require authentication.
const PUBLIC_PATHS: &[&str] = &["/health", "/login", "/register", "/docs"];

/// Check if a request path is public (no auth required).
pub fn is_public_path(path: &str) -> bool {
    let logger = get_logger("middleware.auth");
    let is_public = PUBLIC_PATHS.contains(&path);
    logger.info(&format!("Path {} is public: {}", path, is_public));
    is_public
}

/// Enforce authentication on a request unless the path is public.
pub fn require_auth(request: &Request) -> Result<(), AppError> {
    let logger = get_logger("middleware.auth");
    if is_public_path(&request.path) {
        logger.info("Skipping auth for public path");
        return Ok(());
    }
    let _user = auth_middleware(request)?;
    logger.info("Authentication successful");
    Ok(())
}

/// Enforce admin role on a request.
pub fn require_admin(request: &Request) -> Result<(), AppError> {
    let logger = get_logger("middleware.auth");
    logger.info("Checking admin authorization");
    let user = auth_middleware(request)?;
    if !user.is_admin {
        return Err(AppError::Forbidden("Admin access required".to_string()));
    }
    Ok(())
}
