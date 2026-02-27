"""Background task for cleanup and maintenance."""

import time
from typing import Any, Dict

from ..utils.logging import get_logger
from ..database.connection import DatabaseConnection
from ..database.pool import ConnectionPool
from ..database.queries import SessionQueries
from ..cache.base import BaseCache

_logger = get_logger("tasks.cleanup")

SESSION_MAX_AGE = 86400 * 7  # 7 days
EVENT_MAX_AGE = 86400 * 30   # 30 days


def cleanup_expired_sessions(db: DatabaseConnection) -> int:
    """Remove sessions older than SESSION_MAX_AGE."""
    _logger.info("Cleaning up expired sessions")

    sessions = SessionQueries(db)
    cutoff = time.time() - SESSION_MAX_AGE

    result = db.execute_query(
        "UPDATE sessions SET expired_at = ? WHERE expired_at IS NULL AND created_at < ?",
        (time.strftime("%Y-%m-%dT%H:%M:%SZ"), str(cutoff)),
    )

    _logger.info(f"Expired {result.affected} stale sessions")
    return result.affected


def cleanup_old_events(db: DatabaseConnection) -> int:
    """Remove processed events older than EVENT_MAX_AGE."""
    _logger.info("Cleaning up old events")

    cutoff = time.time() - EVENT_MAX_AGE
    result = db.execute_query(
        "DELETE FROM events WHERE processed_at IS NOT NULL AND created_at < ?",
        (str(cutoff),),
    )

    _logger.info(f"Removed {result.affected} old events")
    return result.affected


def cleanup_cache(cache: BaseCache) -> int:
    """Flush stale cache entries."""
    _logger.info("Running cache cleanup")
    cleared = cache.clear()
    _logger.info(f"Cache cleared: {cleared} entries")
    return cleared


def run_all_cleanup(db: DatabaseConnection, cache: BaseCache) -> Dict[str, int]:
    """Run all cleanup tasks."""
    _logger.info("Running full cleanup cycle")

    sessions = cleanup_expired_sessions(db)
    events = cleanup_old_events(db)
    cache_entries = cleanup_cache(cache)

    pool_stats = {}
    _logger.info("Cleanup complete")

    return {
        "expired_sessions": sessions,
        "old_events": events,
        "cache_cleared": cache_entries,
    }
