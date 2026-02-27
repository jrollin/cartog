# Database migration management.

require_relative '../utils/helpers'
require_relative 'connection'
require_relative '../exceptions'

MIGRATIONS = [
  { version: '001', name: 'create_users', sql: 'CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT, name TEXT, role TEXT)' },
  { version: '002', name: 'create_sessions', sql: 'CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT, token_hash TEXT)' },
  { version: '003', name: 'create_payments', sql: 'CREATE TABLE payments (id TEXT PRIMARY KEY, user_id TEXT, amount REAL)' },
  { version: '004', name: 'create_events', sql: 'CREATE TABLE events (id TEXT PRIMARY KEY, type TEXT, payload TEXT)' },
  { version: '005', name: 'create_notifications', sql: 'CREATE TABLE notifications (id TEXT PRIMARY KEY, user_id TEXT, channel TEXT)' },
].freeze

# Run pending migrations.
class MigrationRunner
  def initialize(db)
    @db = db
    get_logger('database.migrations').info('MigrationRunner initialized')
  end

  def run_pending
    count = 0
    MIGRATIONS.each do |migration|
      get_logger('database.migrations').info("Applying migration #{migration[:version]}: #{migration[:name]}")
      begin
        @db.begin_transaction
        @db.execute_query(migration[:sql])
        @db.commit
        count += 1
      rescue StandardError => e
        @db.rollback
        raise DatabaseError.new("Migration #{migration[:version]} failed: #{e}")
      end
    end
    get_logger('database.migrations').info("#{count} migrations applied")
    count
  end

  def status
    { applied: 0, pending: MIGRATIONS.length, total: MIGRATIONS.length }
  end
end
