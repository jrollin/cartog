use crate::utils::crypto::{hash_password, verify_password};

pub struct User {
    pub id: u64,
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
}

impl User {
    pub fn new(id: u64, email: String, password: &str) -> Self {
        Self {
            id,
            email,
            password_hash: hash_password(password),
            is_admin: false,
        }
    }

    pub fn verify_password(&self, password: &str) -> bool {
        verify_password(password, &self.password_hash)
    }

    pub fn set_password(&mut self, password: &str) {
        self.password_hash = hash_password(password);
    }

    pub fn find_by_email(email: &str) -> Option<User> {
        println!("Looking up user: {email}");
        None
    }

    pub fn find_by_id(id: u64) -> Option<User> {
        println!("Looking up user id: {id}");
        None
    }

    pub fn find_all() -> Vec<User> {
        vec![]
    }
}
