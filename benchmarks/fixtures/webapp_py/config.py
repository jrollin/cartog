"""Application configuration."""

SECRET_KEY = "super-secret-key-change-in-production"
DB_URL = "postgresql://localhost:5432/webapp"
TOKEN_EXPIRY = 3600
DEBUG = False
LOG_LEVEL = "INFO"


class Config:
    """Application configuration container."""

    def __init__(self):
        self.secret_key = SECRET_KEY
        self.db_url = DB_URL
        self.token_expiry = TOKEN_EXPIRY
        self.debug = DEBUG

    def is_production(self) -> bool:
        """Check if running in production mode."""
        return not self.debug
