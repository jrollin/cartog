"""Authentication service — orchestrates login/signup flows."""

from typing import Any, Dict, Optional

from ..utils.logging import get_logger
from ..utils.helpers import validate_request, sanitize_input
from ..database.connection import DatabaseConnection
from ..database.queries import UserQueries, SessionQueries
from ..auth.service import AuthService
from ..auth.tokens import validate_token, generate_token
from ..exceptions import AuthenticationError, ValidationError, NotFoundError
from ..events.dispatcher import EventDispatcher
from .base import BaseService

_logger = get_logger("services.auth_service")


class AuthenticationService(BaseService):
    """High-level authentication service wrapping AuthService with events."""

    def __init__(self, db: DatabaseConnection, event_dispatcher: EventDispatcher):
        super().__init__(db, "authentication")
        self._auth = AuthService(db)
        self._users = UserQueries(db)
        self._sessions = SessionQueries(db)
        self._events = event_dispatcher

    def authenticate(
        self, email: str, password: str, ip_address: str = "unknown"
    ) -> Dict[str, Any]:
        """Authenticate a user and return a session token.

        This is the main entry point for the login flow, called by
        API handlers. It delegates to AuthService.login() for credential
        validation and token generation.
        """
        self._require_initialized()
        _logger.info(f"Authentication attempt for {email}")

        # Sanitize inputs
        clean_email = sanitize_input(email)
        if not clean_email:
            raise ValidationError("Email is required", field="email")

        # Delegate to core auth service
        try:
            token = self._auth.login(clean_email, password)
        except Exception as e:
            _logger.info(f"Authentication failed for {email}: {e}")
            self._events.emit(
                "auth.login_failed", {"email": clean_email, "ip": ip_address}
            )
            raise AuthenticationError(f"Invalid credentials for {email}")

        if not token:
            self._events.emit(
                "auth.login_failed", {"email": clean_email, "ip": ip_address}
            )
            raise AuthenticationError("Invalid credentials")

        # Record session
        user = self._users.find_by_email(clean_email)
        if user:
            self._sessions.create_session(user["id"], token, ip_address)

        self._events.emit(
            "auth.login_success",
            {
                "email": clean_email,
                "ip": ip_address,
            },
        )

        return {"token": token, "email": clean_email}

    def verify_token(self, token: str) -> Optional[Dict[str, Any]]:
        """Verify a token and return the associated user."""
        try:
            claims = validate_token(token)
            if claims and "user_id" in claims:
                return self._users.find_by_email(claims.get("email", ""))
            return None
        except Exception as e:
            _logger.info(f"Token verification failed: {e}")
            return None

    def logout(self, token: str) -> bool:
        """Invalidate a session token."""
        _logger.info("Processing logout")
        session = self._sessions.find_active_session(token)
        if session:
            self._sessions.expire_session(session["id"])
            self._events.emit("auth.logout", {"session_id": session["id"]})
            return True
        return False

    def register(self, email: str, password: str, name: str) -> Dict[str, Any]:
        """Register a new user account."""
        clean_email = sanitize_input(email)
        clean_name = sanitize_input(name)

        # Check for existing user
        existing = self._users.find_by_email(clean_email)
        if existing:
            raise ValidationError("Email already registered", field="email")

        # Create user record
        user_id = self._db.insert(
            "users",
            {
                "email": clean_email,
                "name": clean_name,
                "role": "user",
                "active": 1,
            },
        )

        self._events.emit(
            "auth.user_registered",
            {
                "user_id": user_id,
                "email": clean_email,
            },
        )

        _logger.info(f"User registered: {clean_email}")
        return {"user_id": user_id, "email": clean_email}

    def change_password(
        self, user_id: str, old_password: str, new_password: str
    ) -> bool:
        """Change a user's password after verifying the old one."""
        _logger.info(f"Password change for user {user_id}")

        if len(new_password) < 8:
            raise ValidationError(
                "Password must be at least 8 characters", field="password"
            )

        # Verify old password (delegate to auth service)
        self._auth.change_password(user_id, old_password)

        self._events.emit("auth.password_changed", {"user_id": user_id})
        return True
