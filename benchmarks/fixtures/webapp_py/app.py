"""Main application entry point."""

from config import Config
from routes.auth import login_route, logout_route, refresh_route
from routes.admin import impersonate_route, list_users_route
from utils.logging import get_logger

logger = get_logger(__name__)


def create_app() -> "App":
    """Create and configure the application."""
    config = Config()
    app = App(config)
    register_routes(app)
    logger.info("Application created")
    return app


def register_routes(app: "App"):
    """Register all route handlers."""
    app.route("/login", login_route)
    app.route("/logout", logout_route)
    app.route("/refresh", refresh_route)
    app.route("/admin/impersonate", impersonate_route)
    app.route("/admin/users", list_users_route)


class App:
    """Simple application container."""

    def __init__(self, config: Config):
        self.config = config
        self._routes = {}

    def route(self, path: str, handler):
        """Register a route handler."""
        self._routes[path] = handler

    def handle_request(self, path: str, request: dict):
        """Dispatch a request to the appropriate handler."""
        handler = self._routes.get(path)
        if handler is None:
            raise ValueError(f"No route for {path}")
        return handler(request)
