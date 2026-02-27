# Cleanup background tasks.

require_relative '../utils/helpers'
require_relative '../database/connection'
require_relative '../cache/base_cache'

# Clean up expired sessions.
def cleanup_expired_sessions(db)
  get_logger('tasks.cleanup').info('Cleaning up expired sessions')
  result = db.execute_query(
    'UPDATE sessions SET expired_at = ? WHERE expired_at IS NULL AND created_at < ?',
    [Time.now.to_i, Time.now.to_i - 7 * 86_400]
  )
  get_logger('tasks.cleanup').info("Expired #{result[:affected]} sessions")
  result[:affected]
end

# Clean up old events.
def cleanup_old_events(db)
  get_logger('tasks.cleanup').info('Cleaning up old events')
  result = db.execute_query(
    'DELETE FROM events WHERE processed_at IS NOT NULL AND created_at < ?',
    [Time.now.to_i - 30 * 86_400]
  )
  result[:affected]
end

# Flush cache.
def cleanup_cache(cache)
  get_logger('tasks.cleanup').info('Running cache cleanup')
  cache.clear
end

# Run all cleanup tasks.
def run_all_cleanup(db, cache)
  sessions = cleanup_expired_sessions(db)
  events = cleanup_old_events(db)
  cache_entries = cleanup_cache(cache)
  get_logger('tasks.cleanup').info('Cleanup complete')
  { expired_sessions: sessions, old_events: events, cache_cleared: cache_entries }
end
