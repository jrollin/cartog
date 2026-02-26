use crate::auth::tokens::validate_token;
use crate::error::AppError;
use crate::models::user::User;

use crate::Request;
use crate::Response;

pub fn auth_middleware(request: &Request) -> Result<User, AppError> {
    let token = extract_token(request)
        .ok_or_else(|| AppError::Unauthorized("Missing token".to_string()))?;

    validate_token(&token).map_err(|e| AppError::Unauthorized(e.message))
}

pub fn admin_middleware(request: &Request) -> Result<User, AppError> {
    let user = auth_middleware(request)?;
    if !user.is_admin {
        return Err(AppError::Forbidden("Admin access required".to_string()));
    }
    Ok(user)
}

pub fn extract_token(request: &Request) -> Option<String> {
    for (key, value) in &request.headers {
        if key.to_lowercase() == "authorization" {
            if let Some(token) = value.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }
    None
}
