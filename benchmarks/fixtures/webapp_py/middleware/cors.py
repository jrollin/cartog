"""CORS middleware for cross-origin request handling."""

from typing import Any, Dict, List, Optional

from ..utils.logging import get_logger
from ..utils.helpers import validate_request

_logger = get_logger("middleware.cors")

DEFAULT_ALLOWED_ORIGINS = ["http://localhost:3000", "https://app.example.com"]
DEFAULT_ALLOWED_METHODS = ["GET", "POST", "PUT", "DELETE", "OPTIONS"]
DEFAULT_ALLOWED_HEADERS = ["Content-Type", "Authorization", "X-Request-ID"]


class CorsPolicy:
    """CORS policy configuration."""

    def __init__(self, allowed_origins: Optional[List[str]] = None,
                 allowed_methods: Optional[List[str]] = None,
                 allowed_headers: Optional[List[str]] = None,
                 allow_credentials: bool = True,
                 max_age: int = 86400):
        self.allowed_origins = allowed_origins or DEFAULT_ALLOWED_ORIGINS
        self.allowed_methods = allowed_methods or DEFAULT_ALLOWED_METHODS
        self.allowed_headers = allowed_headers or DEFAULT_ALLOWED_HEADERS
        self.allow_credentials = allow_credentials
        self.max_age = max_age

    def is_origin_allowed(self, origin: str) -> bool:
        """Check if an origin is allowed."""
        if "*" in self.allowed_origins:
            return True
        return origin in self.allowed_origins

    def get_headers(self, origin: str) -> Dict[str, str]:
        """Generate CORS response headers."""
        if not self.is_origin_allowed(origin):
            return {}

        headers = {
            "Access-Control-Allow-Origin": origin,
            "Access-Control-Allow-Methods": ", ".join(self.allowed_methods),
            "Access-Control-Allow-Headers": ", ".join(self.allowed_headers),
            "Access-Control-Max-Age": str(self.max_age),
        }

        if self.allow_credentials:
            headers["Access-Control-Allow-Credentials"] = "true"

        return headers


def cors_middleware(request: Dict[str, Any], policy: Optional[CorsPolicy] = None) -> Dict[str, Any]:
    """Apply CORS headers to a request/response cycle."""
    validate_request(request)
    cors = policy or CorsPolicy()

    origin = request.get("origin", "")
    if origin:
        headers = cors.get_headers(origin)
        request["cors_headers"] = headers
        if not headers:
            _logger.info(f"CORS rejected origin: {origin}")
    else:
        request["cors_headers"] = {}

    return request
