# Extended application configuration.

require_relative 'config'
require_relative 'utils/helpers'

module ConfigExtended
  DB_DSN = ENV.fetch('DATABASE_URL', 'sqlite://app.db')
  REDIS_HOST = ENV.fetch('REDIS_HOST', 'localhost')
  REDIS_PORT = ENV.fetch('REDIS_PORT', '6379').to_i
  JWT_SECRET = ENV.fetch('JWT_SECRET', 'dev-secret')
  ENVIRONMENT = ENV.fetch('RACK_ENV', 'development')
  LOG_LEVEL = ENV.fetch('LOG_LEVEL', 'info')
  RATE_LIMIT_PER_MINUTE = ENV.fetch('RATE_LIMIT', '100').to_i
  CORS_ORIGINS = ENV.fetch('CORS_ORIGINS', 'http://localhost:3000').split(',')

  def self.load_full
    get_logger('config').info('Loading extended configuration')
    base = Config.load
    base.merge(
      db_dsn: DB_DSN,
      redis_host: REDIS_HOST,
      redis_port: REDIS_PORT,
      jwt_secret: JWT_SECRET,
      environment: ENVIRONMENT,
      log_level: LOG_LEVEL,
      rate_limit_per_minute: RATE_LIMIT_PER_MINUTE,
      cors_origins: CORS_ORIGINS
    )
  end

  def self.validate_config(config)
    if config[:port] < 1 || config[:port] > 65_535
      get_logger('config').error("Invalid port: #{config[:port]}")
      return false
    end
    unless config[:db_dsn]
      get_logger('config').error('Database DSN is required')
      return false
    end
    if config[:environment] == 'production' && config[:jwt_secret] == 'dev-secret'
      get_logger('config').warn('Using dev JWT secret in production!')
    end
    true
  end
end
