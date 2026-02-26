"""Authentication service."""

from typing import Optional

from auth.tokens import validate_token, generate_token, revoke_token
from models.user import User
from utils.logging import get_logger


class BaseService:
    """Base service with common utilities."""

    def __init__(self):
        self._initialized = True
        self._logger = get_logger(self.__class__.__name__)

    def _log(self, message: str):
        """Log a message with the service name prefix."""
        self._logger.info(f"[{self.__class__.__name__}] {message}")


class AuthService(BaseService):
    """Handles user authentication flows."""

    def __init__(self, db):
        super().__init__()
        self.db = db

    def login(self, email: str, password: str) -> Optional[str]:
        """Authenticate a user and return a token."""
        user = self._find_user(email)
        if user and user.check_password(password):
            self._log(f"Login successful for {email}")
            return generate_token(user)
        self._log(f"Login failed for {email}")
        return None

    def logout(self, token: str) -> bool:
        """Revoke a user's token."""
        return revoke_token(token)

    def get_current_user(self, token: str) -> Optional[User]:
        """Get the user associated with a token."""
        return validate_token(token)

    def _find_user(self, email: str) -> Optional[User]:
        """Find a user by email."""
        return User.find_by_email(self.db, email)

    def change_password(self, token: str, old_pw: str, new_pw: str) -> bool:
        """Change a user's password after validating their current one."""
        user = validate_token(token)
        if user and user.check_password(old_pw):
            user.set_password(new_pw)
            return True
        return False


class AdminService(AuthService):
    """Extended auth service for admin operations."""

    def impersonate(self, admin_token: str, user_id: int) -> Optional[str]:
        """Allow admin to impersonate another user."""
        admin = self.get_current_user(admin_token)
        if admin and admin.is_admin:
            target = User.find_by_id(self.db, user_id)
            if target:
                self._log(f"Admin {admin.email} impersonating {target.email}")
                return generate_token(target)
        raise PermissionError("Not authorized")

    def list_all_users(self, admin_token: str) -> list:
        """List all users (admin only)."""
        admin = self.get_current_user(admin_token)
        if admin and admin.is_admin:
            return User.find_all(self.db)
        raise PermissionError("Not authorized")
