"""Cacheable service mixin providing caching capabilities."""

import time
from typing import Any, Dict, Optional

from ..utils.logging import get_logger
from ..database.connection import DatabaseConnection
from .base import BaseService

_logger = get_logger("services.cacheable")


class CacheableService(BaseService):
    """Service with built-in caching support.

    Extends BaseService with cache get/set/invalidate operations.
    """

    def __init__(self, db: DatabaseConnection, service_name: str = "cacheable"):
        super().__init__(db, service_name)
        self._cache: Dict[str, Any] = {}
        self._cache_ttl: Dict[str, float] = {}
        self._default_ttl = 300  # 5 minutes
        self._cache_hits = 0
        self._cache_misses = 0

    def cache_get(self, key: str) -> Optional[Any]:
        """Retrieve a value from cache if not expired."""
        if key in self._cache:
            expiry = self._cache_ttl.get(key, 0)
            if time.time() < expiry:
                self._cache_hits += 1
                _logger.info(f"Cache hit: {key}")
                return self._cache[key]
            else:
                # Expired — remove it
                del self._cache[key]
                del self._cache_ttl[key]

        self._cache_misses += 1
        _logger.info(f"Cache miss: {key}")
        return None

    def cache_set(self, key: str, value: Any, ttl: Optional[int] = None) -> None:
        """Store a value in cache with optional TTL."""
        effective_ttl = ttl if ttl is not None else self._default_ttl
        self._cache[key] = value
        self._cache_ttl[key] = time.time() + effective_ttl
        _logger.info(f"Cache set: {key} (ttl={effective_ttl}s)")

    def cache_invalidate(self, key: str) -> bool:
        """Remove a specific key from cache."""
        if key in self._cache:
            del self._cache[key]
            del self._cache_ttl[key]
            _logger.info(f"Cache invalidated: {key}")
            return True
        return False

    def cache_clear(self) -> int:
        """Clear all cached entries."""
        count = len(self._cache)
        self._cache.clear()
        self._cache_ttl.clear()
        _logger.info(f"Cache cleared: {count} entries")
        return count

    def cache_stats(self) -> Dict[str, Any]:
        """Return cache hit/miss statistics."""
        total = self._cache_hits + self._cache_misses
        hit_rate = (self._cache_hits / total * 100) if total > 0 else 0
        return {
            "entries": len(self._cache),
            "hits": self._cache_hits,
            "misses": self._cache_misses,
            "hit_rate": f"{hit_rate:.1f}%",
        }
