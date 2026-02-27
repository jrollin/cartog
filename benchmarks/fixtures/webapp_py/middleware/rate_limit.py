"""Rate limiting middleware."""

import time
from typing import Any, Dict, Optional

from ..utils.logging import get_logger
from ..utils.helpers import validate_request
from ..exceptions import RateLimitError
from ..cache.base import BaseCache

_logger = get_logger("middleware.rate_limit")

DEFAULT_LIMIT = 100
DEFAULT_WINDOW = 60


class RateLimiter:
    """Token-bucket rate limiter backed by cache."""

    def __init__(self, cache: BaseCache, limit: int = DEFAULT_LIMIT,
                 window: int = DEFAULT_WINDOW):
        self._cache = cache
        self._limit = limit
        self._window = window

    def check(self, key: str) -> Dict[str, Any]:
        """Check if a request is within rate limits."""
        cache_key = f"ratelimit:{key}"
        current = self._cache.get(cache_key)

        if current is None:
            self._cache.set(cache_key, 1, ttl=self._window)
            return {"allowed": True, "remaining": self._limit - 1, "limit": self._limit}

        count = int(current)
        if count >= self._limit:
            _logger.info(f"Rate limit exceeded for {key}")
            return {"allowed": False, "remaining": 0, "limit": self._limit}

        self._cache.set(cache_key, count + 1, ttl=self._window)
        return {"allowed": True, "remaining": self._limit - count - 1, "limit": self._limit}


def rate_limit_middleware(request: Dict[str, Any], cache: BaseCache,
                        limit: int = DEFAULT_LIMIT) -> Dict[str, Any]:
    """Apply rate limiting to a request."""
    validate_request(request)
    ip = request.get("ip", "unknown")
    path = request.get("path", "/")
    key = f"{ip}:{path}"

    limiter = RateLimiter(cache, limit=limit)
    result = limiter.check(key)

    if not result["allowed"]:
        raise RateLimitError(retry_after=DEFAULT_WINDOW)

    request["rate_limit"] = result
    return request
