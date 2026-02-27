# Extended application entry point using all services.

require_relative 'utils/helpers'
require_relative 'config_extended'
require_relative 'database/pool'
require_relative 'database/connection'
require_relative 'database/migrations'
require_relative 'events/dispatcher'
require_relative 'events/handlers'
require_relative 'cache/redis_cache'

# Initialize the full application stack.
def initialize_app
  get_logger('app').info('Initializing application')
  config = ConfigExtended.load_full
  unless ConfigExtended.validate_config(config)
    raise 'Invalid configuration'
  end

  # Database
  pool = ConnectionPool.new(config[:db_dsn])
  pool.do_initialize
  db = DatabaseConnection.new(pool)

  # Migrations
  migrations = MigrationRunner.new(db)
  migrations.run_pending

  # Events
  events = EventDispatcher.new
  register_default_handlers(events)

  # Cache
  cache = RedisCache.new(config[:redis_host], config[:redis_port])

  get_logger('app').info('Application initialized')
  { db: db, events: events, cache: cache }
end
