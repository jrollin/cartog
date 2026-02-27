use std::fmt;

use crate::utils::helpers::get_logger;

/// Extended application error types beyond the base AppError.
#[derive(Debug)]
pub enum AppErrorExt {
    /// A validation error with field name and message.
    Validation {
        /// The field that failed validation.
        field: String,
        /// The validation error message.
        message: String,
    },
    /// A payment processing error.
    Payment {
        /// The transaction ID if available.
        transaction_id: Option<String>,
        /// The error message.
        message: String,
    },
    /// A resource was not found.
    NotFound {
        /// The type of resource.
        resource: String,
        /// The identifier that was looked up.
        identifier: String,
    },
    /// Rate limit exceeded.
    RateLimit {
        /// Seconds until the rate limit resets.
        retry_after: u64,
    },
}

impl fmt::Display for AppErrorExt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let logger = get_logger("app_errors");
        match self {
            AppErrorExt::Validation { field, message } => {
                logger.info(&format!("Validation error on field: {}", field));
                write!(f, "Validation error on '{}': {}", field, message)
            }
            AppErrorExt::Payment {
                transaction_id,
                message,
            } => {
                let txn = transaction_id.as_deref().unwrap_or("unknown");
                logger.info(&format!("Payment error for txn: {}", txn));
                write!(f, "Payment error (txn: {}): {}", txn, message)
            }
            AppErrorExt::NotFound {
                resource,
                identifier,
            } => {
                logger.info(&format!("{} not found: {}", resource, identifier));
                write!(f, "{} with id '{}' not found", resource, identifier)
            }
            AppErrorExt::RateLimit { retry_after } => {
                logger.warn(&format!("Rate limited, retry after {}s", retry_after));
                write!(f, "Rate limit exceeded. Retry after {}s", retry_after)
            }
        }
    }
}

impl std::error::Error for AppErrorExt {}

/// Convert an AppErrorExt into an HTTP status code.
pub fn status_code(err: &AppErrorExt) -> u16 {
    match err {
        AppErrorExt::Validation { .. } => 400,
        AppErrorExt::Payment { .. } => 402,
        AppErrorExt::NotFound { .. } => 404,
        AppErrorExt::RateLimit { .. } => 429,
    }
}
