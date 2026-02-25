"""Authentication service."""

from typing import Optional

from .tokens import validate_token, generate_token, revoke_token
from .models import User


class BaseService:
    """Base service with common utilities."""

    def __init__(self):
        self._initialized = True

    def _log(self, message: str):
        print(f"[{self.__class__.__name__}] {message}")


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
        return None

    def logout(self, token: str) -> bool:
        """Revoke a user's token."""
        return revoke_token(token)

    def get_current_user(self, token: str) -> Optional[User]:
        """Get the user associated with a token."""
        return validate_token(token)

    def _find_user(self, email: str) -> Optional[User]:
        """Find a user by email."""
        return self.db.query(User).filter_by(email=email).first()


class AdminService(AuthService):
    """Extended auth service for admin operations."""

    def impersonate(self, admin_token: str, user_id: int) -> Optional[str]:
        """Allow admin to impersonate another user."""
        admin = self.get_current_user(admin_token)
        if admin and admin.is_admin:
            target = self.db.query(User).get(user_id)
            if target:
                return generate_token(target)
        raise PermissionError("Not authorized")
