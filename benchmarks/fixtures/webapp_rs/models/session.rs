use crate::models::user::User;

pub struct Session {
    pub token: String,
    pub user_id: u64,
    pub expires_at: u64,
}

impl Session {
    pub fn create(user: &User, token: String, expires_in: u64) -> Self {
        let expires_at = current_timestamp() + expires_in;
        Self {
            token,
            user_id: user.id,
            expires_at,
        }
    }

    pub fn find_by_token(token: &str) -> Option<Session> {
        println!("Looking up session: {token}");
        None
    }

    pub fn find_all_by_user(user: &User) -> Vec<Session> {
        println!("Looking up sessions for user: {}", user.id);
        vec![]
    }

    pub fn delete(&self) {
        println!("Deleting session: {}", self.token);
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at < current_timestamp()
    }
}

fn current_timestamp() -> u64 {
    0
}
