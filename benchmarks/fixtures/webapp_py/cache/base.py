"""Base cache interface."""

from typing import Any, Dict, Optional

from ..utils.logging import get_logger

_logger = get_logger("cache.base")


class BaseCache:
    """Abstract base class for cache implementations."""

    def __init__(self, name: str = "base"):
        self._name = name
        self._hits = 0
        self._misses = 0

    def get(self, key: str) -> Optional[Any]:
        """Retrieve a value by key. Returns None on miss."""
        raise NotImplementedError

    def set(self, key: str, value: Any, ttl: int = 300) -> None:
        """Store a value with optional TTL in seconds."""
        raise NotImplementedError

    def delete(self, key: str) -> bool:
        """Remove a key. Returns True if existed."""
        raise NotImplementedError

    def clear(self) -> int:
        """Remove all entries. Returns count removed."""
        raise NotImplementedError

    def exists(self, key: str) -> bool:
        """Check if a key exists and is not expired."""
        return self.get(key) is not None

    def stats(self) -> Dict[str, Any]:
        """Return hit/miss statistics."""
        total = self._hits + self._misses
        rate = (self._hits / total * 100) if total > 0 else 0
        return {
            "backend": self._name,
            "hits": self._hits,
            "misses": self._misses,
            "hit_rate": f"{rate:.1f}%",
        }
