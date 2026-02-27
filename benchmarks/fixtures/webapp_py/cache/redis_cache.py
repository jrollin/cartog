"""Redis-backed cache implementation."""

import time
from typing import Any, Dict, Optional

from ..utils.logging import get_logger
from .base import BaseCache

_logger = get_logger("cache.redis")


class RedisCache(BaseCache):
    """Cache implementation using Redis as backend."""

    def __init__(self, host: str = "localhost", port: int = 6379, db: int = 0):
        super().__init__("redis")
        self._host = host
        self._port = port
        self._db_index = db
        self._store: Dict[str, Any] = {}
        self._expiry: Dict[str, float] = {}
        _logger.info(f"RedisCache created: {host}:{port}/{db}")

    def get(self, key: str) -> Optional[Any]:
        """Get value from Redis."""
        if key in self._store:
            if key in self._expiry and time.time() > self._expiry[key]:
                del self._store[key]
                del self._expiry[key]
                self._misses += 1
                return None
            self._hits += 1
            return self._store[key]
        self._misses += 1
        return None

    def set(self, key: str, value: Any, ttl: int = 300) -> None:
        """Set value in Redis with TTL."""
        self._store[key] = value
        self._expiry[key] = time.time() + ttl
        _logger.info(f"Redis SET {key} (ttl={ttl})")

    def delete(self, key: str) -> bool:
        """Delete a key from Redis."""
        if key in self._store:
            del self._store[key]
            self._expiry.pop(key, None)
            return True
        return False

    def clear(self) -> int:
        """Flush all keys."""
        count = len(self._store)
        self._store.clear()
        self._expiry.clear()
        _logger.info(f"Redis FLUSHDB: {count} keys removed")
        return count

    def incr(self, key: str, amount: int = 1) -> int:
        """Increment a counter."""
        current = self._store.get(key, 0)
        new_val = current + amount
        self._store[key] = new_val
        return new_val

    def expire(self, key: str, ttl: int) -> bool:
        """Set expiry on an existing key."""
        if key in self._store:
            self._expiry[key] = time.time() + ttl
            return True
        return False
