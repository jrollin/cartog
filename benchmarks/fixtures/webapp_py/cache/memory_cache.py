"""In-memory LRU cache implementation."""

import time
from typing import Any, Dict, List, Optional, Tuple
from collections import OrderedDict

from ..utils.logging import get_logger
from .base import BaseCache

_logger = get_logger("cache.memory")


class MemoryCache(BaseCache):
    """In-memory cache with LRU eviction."""

    def __init__(self, max_size: int = 1000):
        super().__init__("memory")
        self._max_size = max_size
        self._store: OrderedDict = OrderedDict()
        self._expiry: Dict[str, float] = {}
        _logger.info(f"MemoryCache created: max_size={max_size}")

    def get(self, key: str) -> Optional[Any]:
        """Get value with LRU tracking."""
        if key in self._store:
            if key in self._expiry and time.time() > self._expiry[key]:
                del self._store[key]
                del self._expiry[key]
                self._misses += 1
                return None
            # Move to end (most recently used)
            self._store.move_to_end(key)
            self._hits += 1
            return self._store[key]
        self._misses += 1
        return None

    def set(self, key: str, value: Any, ttl: int = 300) -> None:
        """Set value with LRU eviction."""
        if key in self._store:
            self._store.move_to_end(key)
        elif len(self._store) >= self._max_size:
            evicted_key, _ = self._store.popitem(last=False)
            self._expiry.pop(evicted_key, None)
            _logger.info(f"LRU evicted: {evicted_key}")

        self._store[key] = value
        self._expiry[key] = time.time() + ttl

    def delete(self, key: str) -> bool:
        """Remove a key."""
        if key in self._store:
            del self._store[key]
            self._expiry.pop(key, None)
            return True
        return False

    def clear(self) -> int:
        """Clear all entries."""
        count = len(self._store)
        self._store.clear()
        self._expiry.clear()
        return count

    def size(self) -> int:
        """Return current number of entries."""
        return len(self._store)

    def keys(self) -> List[str]:
        """Return all keys in LRU order."""
        return list(self._store.keys())
