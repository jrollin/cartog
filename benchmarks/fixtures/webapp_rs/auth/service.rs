use crate::auth::tokens::{generate_token, revoke_token, validate_token};
use crate::config::Config;
use crate::error::AppError;
use crate::models::user::User;

pub trait AuthProvider {
    fn login(&self, email: &str, password: &str) -> Result<String, AppError>;
    fn logout(&self, token: &str) -> Result<bool, AppError>;
    fn get_current_user(&self, token: &str) -> Result<User, AppError>;
}

pub struct DefaultAuth {
    config: Config,
}

impl DefaultAuth {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl AuthProvider for DefaultAuth {
    fn login(&self, email: &str, password: &str) -> Result<String, AppError> {
        let user = User::find_by_email(email)
            .ok_or_else(|| AppError::Unauthorized("Invalid credentials".to_string()))?;

        if !user.verify_password(password) {
            return Err(AppError::Unauthorized("Invalid credentials".to_string()));
        }

        let token =
            generate_token(&user, &self.config).map_err(|e| AppError::Internal(e.message))?;

        println!("Login successful for {email}");
        Ok(token.value)
    }

    fn logout(&self, token: &str) -> Result<bool, AppError> {
        Ok(revoke_token(token))
    }

    fn get_current_user(&self, token: &str) -> Result<User, AppError> {
        validate_token(token).map_err(|e| AppError::Unauthorized(e.message))
    }
}

pub struct AdminAuth {
    inner: DefaultAuth,
}

impl AdminAuth {
    pub fn new(config: Config) -> Self {
        Self {
            inner: DefaultAuth::new(config),
        }
    }

    pub fn impersonate(&self, admin_token: &str, user_id: u64) -> Result<String, AppError> {
        let admin = self.inner.get_current_user(admin_token)?;
        if !admin.is_admin {
            return Err(AppError::Forbidden("Not authorized".to_string()));
        }

        let target = User::find_by_id(user_id)
            .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

        let token = generate_token(&target, &self.inner.config)
            .map_err(|e| AppError::Internal(e.message))?;

        println!("Admin {} impersonating user {}", admin.email, target.email);
        Ok(token.value)
    }

    pub fn list_all_users(&self, admin_token: &str) -> Result<Vec<User>, AppError> {
        let admin = self.inner.get_current_user(admin_token)?;
        if !admin.is_admin {
            return Err(AppError::Forbidden("Not authorized".to_string()));
        }
        Ok(User::find_all())
    }
}
