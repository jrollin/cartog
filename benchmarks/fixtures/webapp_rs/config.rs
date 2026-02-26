pub const DEFAULT_PORT: u16 = 8080;
pub const TOKEN_EXPIRY_SECS: u64 = 3600;

pub struct Config {
    pub port: u16,
    pub secret_key: String,
    pub db_url: String,
    pub token_expiry: u64,
}

impl Config {
    pub fn load() -> Self {
        Self {
            port: DEFAULT_PORT,
            secret_key: "super-secret".to_string(),
            db_url: "postgres://localhost/webapp".to_string(),
            token_expiry: TOKEN_EXPIRY_SECS,
        }
    }

    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }

    pub fn is_production(&self) -> bool {
        self.port != DEFAULT_PORT
    }
}
