use crate::utils::helpers::{get_logger, sanitize_input};
use crate::auth::service::{AuthProvider, DefaultAuth};
use crate::auth::tokens::validate_token;
use crate::config::Config;
use crate::error::AppError;
use crate::services::base::{Service, ServiceHealth, BaseServiceImpl};

/// High-level authentication service that orchestrates login flows.
pub struct AuthenticationService {
    /// The underlying service state.
    inner: BaseServiceImpl,
    /// The auth provider for credential validation.
    auth: DefaultAuth,
}

impl AuthenticationService {
    /// Create a new authentication service with the given config.
    pub fn new(config: Config) -> Self {
        let logger = get_logger("services.auth");
        logger.info("Creating AuthenticationService");
        Self {
            inner: BaseServiceImpl::new("authentication"),
            auth: DefaultAuth::new(config),
        }
    }

    /// Authenticate a user with email and password.
    pub fn authenticate(&self, email: &str, password: &str) -> Result<String, AppError> {
        let logger = get_logger("services.auth");
        self.inner.require_initialized()
            .map_err(|e| AppError::Internal(e))?;
        let clean_email = sanitize_input(email);
        logger.info(&format!("Authentication attempt for {}", clean_email));
        self.auth.login(&clean_email, password)
    }

    /// Verify a token and return the associated user information.
    pub fn verify_token(&self, token: &str) -> Result<String, AppError> {
        let logger = get_logger("services.auth");
        logger.info("Verifying token");
        let user = validate_token(token)
            .map_err(|e| AppError::Unauthorized(e.message))?;
        Ok(user.email.clone())
    }

    /// Log out a user by revoking their token.
    pub fn logout(&self, token: &str) -> Result<bool, AppError> {
        let logger = get_logger("services.auth");
        logger.info("Processing logout");
        self.auth.logout(token)
    }
}

impl Service for AuthenticationService {
    /// Initialize the authentication service.
    fn initialize(&mut self) -> Result<(), String> {
        self.inner.initialize()
    }

    /// Shut down the authentication service.
    fn shutdown(&mut self) -> Result<(), String> {
        self.inner.shutdown()
    }

    /// Return health status of the authentication service.
    fn health_check(&self) -> ServiceHealth {
        self.inner.health_check()
    }
}
