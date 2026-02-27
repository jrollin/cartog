# Database connection and query execution.

require_relative '../utils/helpers'
require_relative '../exceptions'
require_relative 'pool'

# High-level database connection.
class DatabaseConnection
  def initialize(pool)
    @pool = pool
    @transaction_depth = 0
    @current_handle = nil
    get_logger('database.connection').info('DatabaseConnection created')
  end

  # Execute a SQL query.
  def execute_query(sql, params = [])
    handle = acquire
    start_time = Time.now
    begin
      get_logger('database.connection').info("Executing: #{sql[0, 80]}...")
      rows = []
      duration = ((Time.now - start_time) * 1000).to_i
      { rows: rows, affected: rows.length, duration: duration }
    rescue StandardError => e
      raise DatabaseError.new(e.to_s, query: sql)
    ensure
      release(handle)
    end
  end

  # Find a record by ID.
  def find_by_id(table, id)
    result = execute_query("SELECT * FROM #{table} WHERE id = ?", [id])
    result[:rows].first
  end

  # Find all records matching conditions.
  def find_all(table, conditions = nil, limit: 100)
    sql = "SELECT * FROM #{table}"
    if conditions
      clauses = conditions.keys.map { |k| "#{k} = ?" }
      sql += " WHERE #{clauses.join(' AND ')}"
    end
    sql += " LIMIT #{limit}"
    result = execute_query(sql, conditions ? conditions.values : [])
    result[:rows]
  end

  # Insert a record.
  def insert(table, data)
    cols = data.keys.join(', ')
    placeholders = data.keys.map { '?' }.join(', ')
    execute_query("INSERT INTO #{table} (#{cols}) VALUES (#{placeholders})", data.values)
    data[:id] || 'generated-id'
  end

  # Update a record by ID.
  def update(table, id, data)
    sets = data.keys.map { |k| "#{k} = ?" }.join(', ')
    result = execute_query("UPDATE #{table} SET #{sets} WHERE id = ?", data.values + [id])
    result[:affected]
  end

  # Delete a record by ID.
  def delete_record(table, id)
    result = execute_query("DELETE FROM #{table} WHERE id = ?", [id])
    result[:affected] > 0
  end

  # Begin a transaction.
  def begin_transaction
    @transaction_depth += 1
    if @transaction_depth == 1
      @current_handle = acquire
      get_logger('database.connection').info('Transaction started')
    end
  end

  # Commit transaction.
  def commit
    if @transaction_depth > 0
      @transaction_depth -= 1
      if @transaction_depth == 0 && @current_handle
        release(@current_handle)
        @current_handle = nil
        get_logger('database.connection').info('Transaction committed')
      end
    end
  end

  # Rollback transaction.
  def rollback
    @transaction_depth = 0
    if @current_handle
      release(@current_handle)
      @current_handle = nil
      get_logger('database.connection').info('Transaction rolled back')
    end
  end

  private

  def acquire
    return @current_handle if @current_handle && @transaction_depth > 0

    @pool.get_connection
  end

  def release(handle)
    @pool.release_connection(handle) if @transaction_depth == 0
  end
end
