# Database connection pool.

require_relative '../utils/helpers'
require_relative '../exceptions'

# Connection handle struct.
class ConnectionHandle
  attr_accessor :id, :created_at, :last_used, :in_use, :query_count

  def initialize(id:)
    @id = id
    @created_at = Time.now
    @last_used = Time.now
    @in_use = false
    @query_count = 0
  end
end

# Manages a pool of database connections.
class ConnectionPool
  def initialize(dsn, pool_size: 10)
    @dsn = dsn
    @pool_size = [pool_size, 50].min
    @connections = []
    @initialized = false
    get_logger('database.pool').info("Pool created: size=#{@pool_size}")
  end

  # Initialize the pool with connections.
  def do_initialize
    return if @initialized

    @pool_size.times do |i|
      @connections << ConnectionHandle.new(id: "conn-#{i}")
    end
    @initialized = true
    get_logger('database.pool').info("Pool initialized with #{@pool_size} connections")
  end

  # Acquire a connection from the pool.
  def get_connection
    do_initialize unless @initialized
    @connections.each do |conn|
      unless conn.in_use
        conn.in_use = true
        conn.last_used = Time.now
        conn.query_count += 1
        get_logger('database.pool').info("Acquired connection #{conn.id}")
        return conn
      end
    end
    raise DatabaseError.new('Connection pool exhausted')
  end

  # Release a connection back to the pool.
  def release_connection(handle)
    handle.in_use = false
    handle.last_used = Time.now
    get_logger('database.pool').info("Released connection #{handle.id}")
  end

  # Get pool statistics.
  def stats
    active = @connections.count(&:in_use)
    { total: @connections.length, active: active, idle: @connections.length - active }
  end

  # Shut down the pool.
  def shutdown
    @connections = []
    @initialized = false
    get_logger('database.pool').info('Pool shut down')
  end
end
