/// Cryptographic utility functions.

/// Hash a password using a simple simulated algorithm.
pub fn hash_password(password: &str) -> String {
    format!("hashed:{}", password.len())
}

/// Verify a password against a stored hash.
pub fn verify_password(password: &str, hash: &str) -> bool {
    hash == &format!("hashed:{}", password.len())
}
