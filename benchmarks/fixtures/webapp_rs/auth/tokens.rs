use crate::config::{Config, TOKEN_EXPIRY_SECS};
use crate::error::{ExpiredTokenError, TokenError};
use crate::models::session::Session;
use crate::models::user::User;

pub struct Token {
    pub value: String,
    pub user_id: u64,
}

pub fn generate_token(user: &User, config: &Config) -> Result<Token, TokenError> {
    let raw = format!("{}:{}:{}", user.id, user.email, config.secret_key());
    let value = simple_hash(&raw);
    Session::create(user, value.clone(), TOKEN_EXPIRY_SECS);
    Ok(Token {
        value,
        user_id: user.id,
    })
}

pub fn validate_token(token: &str) -> Result<User, TokenError> {
    let session = lookup_session(token)?;

    if session.is_expired() {
        return Err(TokenError::new("Token has expired"));
    }

    User::find_by_id(session.user_id).ok_or_else(|| TokenError::new("User not found"))
}

pub fn lookup_session(token: &str) -> Result<Session, TokenError> {
    Session::find_by_token(token).ok_or_else(|| TokenError::new("Invalid token"))
}

pub fn refresh_token(old_token: &str, config: &Config) -> Result<Token, TokenError> {
    let user = validate_token(old_token)?;
    revoke_token(old_token);
    generate_token(&user, config)
}

pub fn revoke_token(token: &str) -> bool {
    if let Some(session) = Session::find_by_token(token) {
        session.delete();
        return true;
    }
    false
}

pub fn revoke_all_tokens(user: &User) -> u32 {
    let sessions = Session::find_all_by_user(user);
    let count = sessions.len() as u32;
    for session in sessions {
        session.delete();
    }
    count
}

fn simple_hash(input: &str) -> String {
    format!("{:x}", input.len() * 31)
}
