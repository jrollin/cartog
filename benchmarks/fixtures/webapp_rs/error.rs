use std::fmt;

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    Unauthorized(String),
    Forbidden(String),
    Internal(String),
    TokenError(TokenError),
}

#[derive(Debug)]
pub struct TokenError {
    pub message: String,
}

#[derive(Debug)]
pub struct ExpiredTokenError {
    pub message: String,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::NotFound(msg) => write!(f, "Not found: {msg}"),
            AppError::Unauthorized(msg) => write!(f, "Unauthorized: {msg}"),
            AppError::Forbidden(msg) => write!(f, "Forbidden: {msg}"),
            AppError::Internal(msg) => write!(f, "Internal error: {msg}"),
            AppError::TokenError(e) => write!(f, "Token error: {}", e.message),
        }
    }
}

impl From<TokenError> for AppError {
    fn from(err: TokenError) -> Self {
        AppError::TokenError(err)
    }
}

impl From<ExpiredTokenError> for AppError {
    fn from(err: ExpiredTokenError) -> Self {
        AppError::TokenError(TokenError {
            message: err.message,
        })
    }
}

impl TokenError {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl ExpiredTokenError {
    pub fn new() -> Self {
        Self {
            message: "Token has expired".to_string(),
        }
    }
}
