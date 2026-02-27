#!/usr/bin/env python3
"""Generate Ruby benchmark fixture files (~5-7K LOC) to expand webapp_rb/.

IMPORTANT: Does NOT overwrite existing files. Only creates new ones.

Existing files (preserved):
  app.rb, config.rb, auth/service.rb, auth/tokens.rb, auth/middleware.rb,
  models/user.rb, models/session.rb, routes/auth.rb, routes/admin.rb,
  utils/logging.rb
"""

import os
import textwrap

BASE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "webapp_rb")

EXISTING_FILES = {
    "app.rb",
    "config.rb",
    "auth/service.rb",
    "auth/tokens.rb",
    "auth/middleware.rb",
    "models/user.rb",
    "models/session.rb",
    "routes/auth.rb",
    "routes/admin.rb",
    "utils/logging.rb",
}


def w(path, content):
    """Write a file only if it does not already exist."""
    if path in EXISTING_FILES:
        print(f"  SKIP (existing): {path}")
        return
    full = os.path.join(BASE, path)
    if os.path.exists(full):
        print(f"  SKIP (on disk):  {path}")
        return
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w") as f:
        f.write(textwrap.dedent(content).lstrip())
    print(f"  CREATED: {path}")


# ─── utils/helpers.rb ───
w(
    "utils/helpers.rb",
    """\
    # Shared utility helpers.

    require_relative 'logging'

    # Get a named logger instance.
    def get_logger(name)
      Logging.get_logger(name)
    end

    # Validate that a request hash has required fields.
    def validate_request(request)
      raise ArgumentError, 'Request must be a Hash' unless request.is_a?(Hash)

      %i[method path].each do |field|
        raise ArgumentError, "Missing required field: #{field}" unless request.key?(field)
      end
      true
    end

    # Generate a unique request identifier.
    def generate_request_id
      ts = (Time.now.to_f * 1000).to_i
      rand_part = SecureRandom.hex(4)
      "req-#{ts}-#{rand_part}"
    end

    # Sanitize user input by removing control characters.
    def sanitize_input(value)
      return '' if value.nil? || value.empty?

      value.gsub(/[\\x00-\\x1f]/, '').strip
    end

    # Paginate a list of items.
    def paginate(items, page: 1, per_page: 20)
      total = items.length
      start_idx = (page - 1) * per_page
      page_items = items[start_idx, per_page] || []
      {
        items: page_items,
        page: page,
        per_page: per_page,
        total: total,
        pages: (total.to_f / per_page).ceil
      }
    end

    # Mask sensitive fields in a hash for logging.
    def mask_sensitive(data, fields)
      masked = data.dup
      fields.each do |field|
        if masked.key?(field)
          val = masked[field].to_s
          masked[field] = val.length > 4 ? "#{val[0, 2]}***#{val[-2, 2]}" : '***'
        end
      end
      masked
    end

    # Retry an operation with exponential backoff.
    def retry_operation(max_retries: 3, delay: 1.0)
      last_error = nil
      max_retries.times do |attempt|
        begin
          return yield
        rescue StandardError => e
          last_error = e
          sleep(delay * (2**attempt))
        end
      end
      raise last_error
    end
    """,
)

# ─── exceptions.rb ───
w(
    "exceptions.rb",
    """\
    # Application error hierarchy.

    # Base application error.
    class AppError < StandardError
      attr_reader :code

      def initialize(message = 'Application error', code: 500)
        @code = code
        super(message)
      end

      def to_h
        { error: self.class.name, message: message, code: @code }
      end
    end

    # Raised when input validation fails.
    class ValidationError < AppError
      attr_reader :field

      def initialize(message = 'Validation failed', field: nil)
        @field = field
        super(message, code: 400)
      end
    end

    # Raised when a payment operation fails.
    class PaymentError < AppError
      attr_reader :transaction_id

      def initialize(message = 'Payment failed', transaction_id: nil)
        @transaction_id = transaction_id
        super(message, code: 402)
      end
    end

    # Raised when a resource is not found.
    class NotFoundError < AppError
      attr_reader :resource, :identifier

      def initialize(resource, identifier)
        @resource = resource
        @identifier = identifier
        super("#{resource} with id '#{identifier}' not found", code: 404)
      end
    end

    # Raised when rate limit is exceeded.
    class RateLimitError < AppError
      attr_reader :retry_after

      def initialize(retry_after: 60)
        @retry_after = retry_after
        super("Rate limit exceeded. Retry after #{retry_after}s", code: 429)
      end
    end

    # Raised when authentication fails.
    class AuthenticationError < AppError
      def initialize(message = 'Authentication required')
        super(message, code: 401)
      end
    end

    # Raised when authorization fails.
    class AuthorizationError < AppError
      def initialize(action, resource)
        super("Not authorized to #{action} on #{resource}", code: 403)
      end
    end

    # Raised when a database operation fails.
    class DatabaseError < AppError
      attr_reader :query

      def initialize(message = 'Database error', query: nil)
        @query = query
        super(message, code: 500)
      end
    end
    """,
)

# ─── database/connection.rb ───
w(
    "database/connection.rb",
    """\
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
    """,
)

# ─── database/pool.rb ───
w(
    "database/pool.rb",
    """\
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
    """,
)

# ─── database/queries.rb ───
w(
    "database/queries.rb",
    """\
    # Predefined query builders.

    require_relative '../utils/helpers'
    require_relative 'connection'

    # User queries.
    class UserQueries
      def initialize(db)
        @db = db
      end

      def find_by_email(email)
        get_logger('database.queries').info("Finding user by email: #{email}")
        result = @db.execute_query('SELECT * FROM users WHERE email = ?', [email])
        result[:rows].first
      end

      def find_active_users(limit = 100)
        result = @db.execute_query('SELECT * FROM users WHERE active = 1 LIMIT ?', [limit])
        result[:rows]
      end

      def search_users(query)
        @db.execute_query('SELECT * FROM users WHERE name LIKE ? OR email LIKE ?', ["%#{query}%", "%#{query}%"])
      end

      def soft_delete(user_id)
        get_logger('database.queries').info("Soft-deleting user #{user_id}")
        affected = @db.update('users', user_id, { deleted_at: Time.now.to_i })
        affected > 0
      end
    end

    # Session queries.
    class SessionQueries
      def initialize(db)
        @db = db
      end

      def find_active_session(token)
        result = @db.execute_query('SELECT * FROM sessions WHERE token_hash = ?', [token])
        result[:rows].first
      end

      def create_session(user_id, token_hash, ip)
        get_logger('database.queries').info("Creating session for user #{user_id}")
        @db.insert('sessions', { user_id: user_id, token_hash: token_hash, ip_address: ip, created_at: Time.now.to_i })
      end

      def expire_session(session_id)
        affected = @db.update('sessions', session_id, { expired_at: Time.now.to_i })
        affected > 0
      end
    end

    # Payment queries.
    class PaymentQueries
      def initialize(db)
        @db = db
      end

      def find_by_transaction_id(txn_id)
        result = @db.execute_query('SELECT * FROM payments WHERE transaction_id = ?', [txn_id])
        result[:rows].first
      end

      def find_user_payments(user_id, status = nil)
        get_logger('database.queries').info("Finding payments for user #{user_id}")
        if status
          result = @db.execute_query('SELECT * FROM payments WHERE user_id = ? AND status = ?', [user_id, status])
          return result[:rows]
        end
        result = @db.execute_query('SELECT * FROM payments WHERE user_id = ?', [user_id])
        result[:rows]
      end

      def create_payment(user_id, amount, currency, txn_id)
        @db.insert('payments', {
          user_id: user_id, amount: amount, currency: currency,
          transaction_id: txn_id, status: 'pending', created_at: Time.now.to_i
        })
      end

      def update_status(txn_id, status)
        get_logger('database.queries').info("Updating payment #{txn_id} to #{status}")
        result = @db.execute_query('UPDATE payments SET status = ? WHERE transaction_id = ?', [status, txn_id])
        result[:affected] > 0
      end

      def calculate_revenue(start_date, end_date)
        result = @db.execute_query(
          "SELECT SUM(amount) as total FROM payments WHERE status = 'completed' AND created_at BETWEEN ? AND ?",
          [start_date, end_date]
        )
        row = result[:rows].first
        row ? (row[:total] || 0).to_f : 0.0
      end
    end
    """,
)

# ─── database/migrations.rb ───
w(
    "database/migrations.rb",
    """\
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
    """,
)

# ─── services/base.rb ───
w(
    "services/base.rb",
    """\
    # Service base class for the services layer.

    require_relative '../utils/helpers'
    require_relative '../database/connection'

    # Re-export BaseService from auth/service for service-layer use.
    # BaseService is defined in auth/service.rb. This file provides the
    # Auditable module for mixin-based audit logging.

    # Auditable mixin for services that need audit trails.
    module Auditable
      def record_audit(action, actor, resource, details = {})
        @audit_log ||= []
        @audit_log << {
          action: action,
          actor: actor,
          resource: resource,
          details: details,
          timestamp: Time.now.to_i
        }
        get_logger('auditable').info("Audit: #{actor} #{action} on #{resource}")
      end

      def get_audit_trail(resource = nil, limit = 50)
        @audit_log ||= []
        entries = @audit_log
        entries = entries.select { |e| e[:resource] == resource } if resource
        entries.last(limit).reverse
      end
    end
    """,
)

# ─── services/cacheable.rb ───
w(
    "services/cacheable.rb",
    """\
    # Cacheable module for services with built-in caching.

    require_relative '../utils/helpers'

    # Mixin providing cache get/set/invalidate to any service.
    module Cacheable
      def cache_get(key)
        @cache_store ||= {}
        entry = @cache_store[key]
        if entry && Time.now.to_i < entry[:expiry]
          get_logger('cacheable').info("Cache hit: #{key}")
          return entry[:value]
        end
        @cache_store.delete(key) if entry
        get_logger('cacheable').info("Cache miss: #{key}")
        nil
      end

      def cache_set(key, value, ttl = 300)
        @cache_store ||= {}
        @cache_store[key] = { value: value, expiry: Time.now.to_i + ttl }
        get_logger('cacheable').info("Cache set: #{key} (ttl=#{ttl}s)")
      end

      def cache_invalidate(key)
        @cache_store ||= {}
        !@cache_store.delete(key).nil?
      end

      def cache_clear
        @cache_store ||= {}
        count = @cache_store.size
        @cache_store.clear
        get_logger('cacheable').info("Cache cleared: #{count} entries")
        count
      end
    end
    """,
)

# ─── services/auditable.rb ───
w(
    "services/auditable.rb",
    """\
    # Auditable service with standalone audit trail.

    require_relative '../utils/helpers'
    require_relative '../auth/service'
    require_relative 'base'

    # Service with audit logging support.
    class AuditableService < BaseService
      include Auditable

      def initialize
        super
        @audit_log = []
      end
    end
    """,
)

# ─── services/auth_service.rb ───
w(
    "services/auth_service.rb",
    """\
    # High-level authentication service.

    require_relative '../utils/helpers'
    require_relative '../auth/service'
    require_relative '../auth/tokens'
    require_relative '../database/connection'
    require_relative '../database/queries'
    require_relative '../events/dispatcher'
    require_relative '../exceptions'

    # Orchestrates authentication flows.
    class AuthenticationService < BaseService
      def initialize(db, events)
        super()
        @auth = AuthService.new(db)
        @users = UserQueries.new(db)
        @sessions = SessionQueries.new(db)
        @events = events
      end

      # Authenticate a user — main entry point for login flow.
      def authenticate(email, password, ip = 'unknown')
        _log("Authentication attempt for #{email}")
        clean_email = sanitize_input(email)
        raise ValidationError.new('Email is required', field: :email) if clean_email.empty?

        begin
          token = @auth.login(clean_email, password)
          unless token
            @events.emit('auth.login_failed', { email: clean_email, ip: ip })
            raise AuthenticationError.new('Invalid credentials')
          end
          @events.emit('auth.login_success', { email: clean_email, ip: ip })
          { token: token, email: clean_email }
        rescue AuthenticationError
          raise
        rescue StandardError => e
          @events.emit('auth.login_failed', { email: clean_email, ip: ip })
          raise AuthenticationError.new("Authentication failed: #{e}")
        end
      end

      # Verify a token.
      def verify_token(token)
        begin
          user = validate_token(token)
          @users.find_by_email(user.email) if user
        rescue TokenError
          nil
        end
      end

      # Log out.
      def do_logout(token)
        _log('Processing logout')
        session = @sessions.find_active_session(token)
        if session
          @sessions.expire_session(session[:id])
          @events.emit('auth.logout', { session_id: session[:id] })
          return true
        end
        false
      end
    end
    """,
)

# ─── services/payment/processor.rb ───
w(
    "services/payment/processor.rb",
    """\
    # Payment processor with caching + audit mixins.

    require_relative '../../utils/helpers'
    require_relative '../cacheable'
    require_relative '../base'
    require_relative '../../auth/service'
    require_relative '../../database/connection'
    require_relative '../../database/queries'
    require_relative '../../events/dispatcher'
    require_relative '../../exceptions'

    SUPPORTED_CURRENCIES = %w[USD EUR GBP JPY CAD].freeze

    # Payment processor with caching and audit trail.
    class PaymentProcessor < BaseService
      include Cacheable
      include Auditable

      def initialize(db, events)
        super()
        @events = events
        @queries = PaymentQueries.new(db)
      end

      # Process a payment.
      def process_payment(user_id, amount, currency, method = 'card')
        _log("Processing payment: user=#{user_id}, amount=#{amount} #{currency}")
        validate_payment(amount, currency)
        txn_id = generate_request_id
        cache_key = "payment:#{user_id}:#{amount}:#{currency}"
        if cache_get(cache_key)
          raise PaymentError.new('Duplicate payment', transaction_id: txn_id)
        end
        begin
          @queries.create_payment(user_id, amount, currency, txn_id)
          @queries.update_status(txn_id, 'completed')
        rescue StandardError => e
          raise PaymentError.new("Payment failed: #{e}", transaction_id: txn_id)
        end
        cache_set(cache_key, txn_id, 300)
        record_audit('payment.processed', user_id, "payment:#{txn_id}", { amount: amount, currency: currency, method: method })
        @events.emit('payment.completed', { transaction_id: txn_id, user_id: user_id, amount: amount, currency: currency })
        { transaction_id: txn_id, status: 'completed', amount: amount, currency: currency }
      end

      # Refund a payment.
      def refund(transaction_id, reason = '')
        _log("Refunding: #{transaction_id}")
        payment = @queries.find_by_transaction_id(transaction_id)
        raise NotFoundError.new('Payment', transaction_id) unless payment

        @queries.update_status(transaction_id, 'refunded')
        record_audit('payment.refunded', 'system', "payment:#{transaction_id}", { reason: reason })
        @events.emit('payment.refunded', { transaction_id: transaction_id, reason: reason })
        { transaction_id: transaction_id, status: 'refunded' }
      end

      private

      def validate_payment(amount, currency)
        unless SUPPORTED_CURRENCIES.include?(currency)
          raise ValidationError.new("Unsupported currency: #{currency}", field: :currency)
        end
        raise ValidationError.new('Amount must be positive', field: :amount) if amount <= 0
        raise ValidationError.new('Amount exceeds maximum', field: :amount) if amount > 999_999
      end
    end
    """,
)

# ─── services/payment/gateway.rb ───
w(
    "services/payment/gateway.rb",
    """\
    # Payment gateway abstraction.

    require_relative '../../utils/helpers'

    # Payment gateway client.
    class PaymentGateway
      def initialize(api_key, environment = 'sandbox')
        @api_key = api_key
        @environment = environment
        @request_count = 0
        get_logger('services.payment.gateway').info("Gateway initialized: env=#{environment}")
      end

      # Charge a payment source.
      def charge(amount, currency, source)
        get_logger('services.payment.gateway').info("Charging #{amount} #{currency}")
        @request_count += 1
        txn_id = generate_request_id
        return { success: false, txn_id: txn_id, message: 'Exceeds limit' } if amount > 10_000

        { success: true, txn_id: txn_id, message: 'Charge successful' }
      end

      # Refund a charge.
      def refund_charge(charge_id)
        get_logger('services.payment.gateway').info("Refunding charge #{charge_id}")
        @request_count += 1
        { success: true, txn_id: generate_request_id, message: 'Refund successful' }
      end

      # Get request statistics.
      def stats
        { total_requests: @request_count }
      end
    end
    """,
)

# ─── services/notification/manager.rb ───
w(
    "services/notification/manager.rb",
    """\
    # Notification management.

    require_relative '../../utils/helpers'
    require_relative '../../auth/service'
    require_relative '../../database/connection'
    require_relative '../../exceptions'

    # Manages notifications across channels.
    class NotificationManager < BaseService
      VALID_CHANNELS = %w[email sms push in_app].freeze

      def initialize(db)
        super()
        @db = db
        @queue = []
      end

      # Send a notification.
      def send_notification(user_id, channel, subject, body)
        _log("Queuing notification for #{user_id} via #{channel}")
        unless VALID_CHANNELS.include?(channel)
          raise ValidationError.new("Invalid channel: #{channel}", field: :channel)
        end
        notification = {
          user_id: user_id,
          channel: channel,
          subject: sanitize_input(subject),
          body: sanitize_input(body),
          status: 'pending',
          created_at: Time.now.to_i
        }
        @queue << notification
        @db.insert('notifications', notification)
        notification
      end

      # Process the notification queue.
      def process_queue
        _log("Processing #{@queue.length} notifications")
        sent = 0
        failed = 0
        @queue.each do |n|
          if n[:status] == 'pending'
            begin
              n[:status] = 'sent'
              sent += 1
            rescue StandardError
              n[:status] = 'failed'
              failed += 1
            end
          end
        end
        @queue.reject! { |n| n[:status] != 'pending' }
        { sent: sent, failed: failed }
      end
    end
    """,
)

# ─── services/email/sender.rb ───
w(
    "services/email/sender.rb",
    """\
    # Email sending service.

    require_relative '../../utils/helpers'
    require_relative '../cacheable'
    require_relative '../../auth/service'
    require_relative '../../database/connection'
    require_relative '../../exceptions'

    TEMPLATES = {
      'welcome' => 'Welcome to our platform, {name}!',
      'password_reset' => 'Reset your password: {link}',
      'payment_receipt' => 'Payment of {amount} {currency} received. Txn: {txn_id}',
    }.freeze

    # Email sender with template support.
    class EmailSender < BaseService
      include Cacheable

      def initialize(db)
        super()
        @db = db
        @sent_count = 0
        @failed_count = 0
      end

      # Send a single email.
      def send_email(to, subject, body)
        _log("Sending email to #{to}: #{subject}")
        raise ValidationError.new('Invalid email', field: :to) unless to.include?('@')

        begin
          @db.insert('notifications', { user_id: 'system', channel: 'email', subject: subject, body: body, status: 'sent' })
          @sent_count += 1
          true
        rescue StandardError => e
          get_logger('services.email').error("Email failed: #{e}")
          @failed_count += 1
          false
        end
      end

      # Send using a template.
      def send_template(to, template_name, context)
        template = TEMPLATES[template_name]
        raise ValidationError.new("Unknown template: #{template_name}", field: :template) unless template

        body = template.dup
        context.each { |key, val| body.gsub!("{#{key}}", val.to_s) }
        subject = "[App] #{template_name.tr('_', ' ')}"
        send_email(to, subject, body)
      end

      # Get sending statistics.
      def stats
        { sent: @sent_count, failed: @failed_count }
      end
    end
    """,
)

# ─── events/dispatcher.rb ───
w(
    "events/dispatcher.rb",
    """\
    # Event dispatcher.

    require_relative '../utils/helpers'

    # Central event bus.
    class EventDispatcher
      def initialize
        @handlers = {}
        @event_log = []
      end

      # Register a handler for an event type.
      def on(event_type, &handler)
        @handlers[event_type] ||= []
        @handlers[event_type] << handler
        get_logger('events.dispatcher').info("Handler registered for: #{event_type}")
      end

      # Emit an event to all registered handlers.
      def emit(event_type, data = {})
        event = { type: event_type, data: data, timestamp: Time.now.to_i, processed: false }
        @event_log << event
        handlers = @handlers[event_type] || []
        get_logger('events.dispatcher').info("Emitting #{event_type} to #{handlers.length} handlers")
        invoked = 0
        handlers.each do |handler|
          begin
            handler.call(event)
            invoked += 1
          rescue StandardError => e
            get_logger('events.dispatcher').error("Handler error for #{event_type}: #{e}")
          end
        end
        event[:processed] = true
        invoked
      end

      # Get total event count.
      def event_count
        @event_log.length
      end
    end
    """,
)

# ─── events/handlers.rb ───
w(
    "events/handlers.rb",
    """\
    # Default event handlers.

    require_relative '../utils/helpers'
    require_relative 'dispatcher'

    # Log when a user registers.
    def on_user_registered(event)
      get_logger('events.handlers').info("User registered: #{event[:data][:email]}")
    end

    # Log successful logins.
    def on_login_success(event)
      get_logger('events.handlers').info("Login success: #{event[:data][:email]} from #{event[:data][:ip]}")
    end

    # Log failed logins.
    def on_login_failed(event)
      get_logger('events.handlers').info("Login failed: #{event[:data][:email]} from #{event[:data][:ip]}")
    end

    # Log completed payments.
    def on_payment_completed(event)
      get_logger('events.handlers').info("Payment completed: txn=#{event[:data][:transaction_id]} amount=#{event[:data][:amount]}")
    end

    # Log refunded payments.
    def on_payment_refunded(event)
      get_logger('events.handlers').info("Payment refunded: txn=#{event[:data][:transaction_id]}")
    end

    # Register all default event handlers.
    def register_default_handlers(dispatcher)
      dispatcher.on('auth.user_registered') { |e| on_user_registered(e) }
      dispatcher.on('auth.login_success') { |e| on_login_success(e) }
      dispatcher.on('auth.login_failed') { |e| on_login_failed(e) }
      dispatcher.on('payment.completed') { |e| on_payment_completed(e) }
      dispatcher.on('payment.refunded') { |e| on_payment_refunded(e) }
      get_logger('events.handlers').info('Default handlers registered')
    end
    """,
)

# ─── cache/base_cache.rb ───
w(
    "cache/base_cache.rb",
    """\
    # Cache interface and base class.

    # Base cache with stats tracking.
    class BaseCache
      attr_reader :name

      def initialize(name)
        @name = name
        @hits = 0
        @misses = 0
      end

      def get(key)
        raise NotImplementedError, 'Subclass must implement get'
      end

      def set(key, value, ttl = 300)
        raise NotImplementedError, 'Subclass must implement set'
      end

      def delete(key)
        raise NotImplementedError, 'Subclass must implement delete'
      end

      def clear
        raise NotImplementedError, 'Subclass must implement clear'
      end

      def stats
        total = @hits + @misses
        rate = total > 0 ? (@hits.to_f / total * 100) : 0.0
        { backend: @name, hits: @hits, misses: @misses, hit_rate: "#{rate.round(1)}%" }
      end
    end
    """,
)

# ─── cache/redis_cache.rb ───
w(
    "cache/redis_cache.rb",
    """\
    # Redis-backed cache.

    require_relative '../utils/helpers'
    require_relative 'base_cache'

    # Redis cache implementation.
    class RedisCache < BaseCache
      def initialize(host = 'localhost', port = 6379)
        super('redis')
        @store = {}
        @expiry = {}
        get_logger('cache.redis').info("RedisCache created: #{host}:#{port}")
      end

      def get(key)
        if @store.key?(key)
          exp = @expiry[key] || Float::INFINITY
          if Time.now.to_i > exp
            @store.delete(key)
            @expiry.delete(key)
            @misses += 1
            return nil
          end
          @hits += 1
          return @store[key]
        end
        @misses += 1
        nil
      end

      def set(key, value, ttl = 300)
        @store[key] = value
        @expiry[key] = Time.now.to_i + ttl
        get_logger('cache.redis').info("Redis SET #{key} (ttl=#{ttl})")
      end

      def delete(key)
        @expiry.delete(key)
        !@store.delete(key).nil?
      end

      def clear
        count = @store.size
        @store.clear
        @expiry.clear
        get_logger('cache.redis').info("Redis FLUSHDB: #{count} keys")
        count
      end

      def incr(key, amount = 1)
        current = @store[key] || 0
        new_val = current + amount
        @store[key] = new_val
        new_val
      end
    end
    """,
)

# ─── cache/memory_cache.rb ───
w(
    "cache/memory_cache.rb",
    """\
    # In-memory LRU cache.

    require_relative '../utils/helpers'
    require_relative 'base_cache'

    # LRU memory cache.
    class MemoryCache < BaseCache
      def initialize(max_size = 1000)
        super('memory')
        @store = {}
        @expiry = {}
        @max_size = max_size
        get_logger('cache.memory').info("MemoryCache created: max_size=#{max_size}")
      end

      def get(key)
        if @store.key?(key)
          exp = @expiry[key] || Float::INFINITY
          if Time.now.to_i > exp
            @store.delete(key)
            @expiry.delete(key)
            @misses += 1
            return nil
          end
          @hits += 1
          # Move to end for LRU
          val = @store.delete(key)
          @store[key] = val
          return val
        end
        @misses += 1
        nil
      end

      def set(key, value, ttl = 300)
        if @store.key?(key)
          @store.delete(key)
        elsif @store.size >= @max_size
          first_key = @store.keys.first
          if first_key
            @store.delete(first_key)
            @expiry.delete(first_key)
            get_logger('cache.memory').info("LRU evicted: #{first_key}")
          end
        end
        @store[key] = value
        @expiry[key] = Time.now.to_i + ttl
      end

      def delete(key)
        @expiry.delete(key)
        !@store.delete(key).nil?
      end

      def clear
        count = @store.size
        @store.clear
        @expiry.clear
        count
      end

      def size
        @store.size
      end
    end
    """,
)

# ─── validators/common.rb ───
w(
    "validators/common.rb",
    """\
    # Common validation utilities.

    require_relative '../utils/helpers'
    require_relative '../exceptions'

    EMAIL_REGEX = /\\A[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}\\z/

    # Validate email format.
    def validate_email(email)
      raise ValidationError.new('Email is required', field: :email) unless email && !email.empty?

      clean = email.strip.downcase
      unless EMAIL_REGEX.match?(clean)
        raise ValidationError.new("Invalid email: #{email}", field: :email)
      end
      clean
    end

    # Validate string length.
    def validate_string(value, field, min_len: 1, max_len: 255)
      raise ValidationError.new("#{field} is required", field: field) unless value && !value.empty?

      stripped = value.strip
      raise ValidationError.new("#{field} too short", field: field) if stripped.length < min_len
      raise ValidationError.new("#{field} too long", field: field) if stripped.length > max_len

      stripped
    end

    # Validate positive number.
    def validate_positive_number(value, field)
      num = value.to_f
      raise ValidationError.new("#{field} must be a number", field: field) if num.zero? && value.to_s != '0'
      raise ValidationError.new("#{field} must be positive", field: field) if num <= 0

      num
    end

    # Validate enum value.
    def validate_enum(value, allowed, field)
      unless allowed.include?(value)
        raise ValidationError.new("Invalid #{field}: '#{value}'. Allowed: #{allowed.join(', ')}", field: field)
      end
      value
    end
    """,
)

# ─── validators/user.rb ───
w(
    "validators/user.rb",
    """\
    # User input validation.

    require_relative '../utils/helpers'
    require_relative '../exceptions'
    require_relative 'common'

    module UserValidator
      # Validate user data — name collision with PaymentValidator, ApiV1Auth, ApiV2Auth.
      def self.validate(data)
        get_logger('validators.user').info('Validating user data')
        raise ValidationError.new('Email required', field: :email) unless data[:email]
        raise ValidationError.new('Name required', field: :name) unless data[:name]

        result = {}
        result[:email] = validate_email(data[:email])
        result[:name] = validate_string(data[:name], :name, min_len: 1, max_len: 100)
        if data[:password]
          raise ValidationError.new('Password too short', field: :password) if data[:password].length < 8

          result[:password] = data[:password]
        end
        if data[:role]
          allowed = %w[user admin moderator]
          unless allowed.include?(data[:role])
            raise ValidationError.new('Invalid role', field: :role)
          end

          result[:role] = data[:role]
        end
        result
      end

      # Validate login data.
      def self.validate_login(data)
        raise ValidationError.new('Email required', field: :email) unless data[:email]
        raise ValidationError.new('Password required', field: :password) unless data[:password]

        { email: validate_email(data[:email]), password: data[:password] }
      end
    end
    """,
)

# ─── validators/payment.rb ───
w(
    "validators/payment.rb",
    """\
    # Payment input validation.

    require_relative '../utils/helpers'
    require_relative '../exceptions'
    require_relative 'common'

    PAYMENT_CURRENCIES = %w[USD EUR GBP JPY CAD].freeze
    PAYMENT_METHODS = %w[card bank_transfer wallet].freeze

    module PaymentValidator
      # Validate payment data — name collision with UserValidator, ApiV1Auth, ApiV2Auth.
      def self.validate(data)
        get_logger('validators.payment').info('Validating payment data')
        raise ValidationError.new('Amount required', field: :amount) unless data[:amount]
        raise ValidationError.new('Currency required', field: :currency) unless data[:currency]
        raise ValidationError.new('User ID required', field: :user_id) unless data[:user_id]

        result = {}
        result[:amount] = validate_positive_number(data[:amount], :amount)
        result[:currency] = validate_enum(data[:currency], PAYMENT_CURRENCIES, :currency)
        result[:user_id] = data[:user_id]
        result[:payment_method] = data[:payment_method] ? validate_enum(data[:payment_method], PAYMENT_METHODS, :payment_method) : 'card'
        result
      end

      # Validate refund data.
      def self.validate_refund(data)
        raise ValidationError.new('Transaction ID required', field: :transaction_id) unless data[:transaction_id]

        result = { transaction_id: data[:transaction_id] }
        result[:amount] = validate_positive_number(data[:amount], :amount) if data[:amount]
        result[:reason] = data[:reason].to_s[0, 500] if data[:reason]
        result
      end
    end
    """,
)

# ─── middleware/auth_middleware.rb ───
w(
    "middleware/auth_middleware.rb",
    """\
    # Auth middleware (services layer).

    require_relative '../utils/helpers'
    require_relative '../auth/tokens'
    require_relative '../auth/middleware'
    require_relative '../exceptions'

    # Authentication middleware for the services layer.
    class AuthMiddleware
      PUBLIC_PATHS = %w[/health /login /register].freeze

      def initialize(app)
        @app = app
      end

      def call(request)
        validate_request(request)
        return @app.call(request) if PUBLIC_PATHS.include?(request[:path])

        token = extract_token(request)
        raise AuthenticationError.new('Missing token') unless token

        begin
          user = validate_token(token)
          request[:user] = user
          request[:authenticated] = true
          @app.call(request)
        rescue TokenError
          get_logger('middleware.auth').warn('Token validation failed')
          raise AuthenticationError.new('Invalid token')
        end
      end
    end
    """,
)

# ─── middleware/rate_limit.rb ───
w(
    "middleware/rate_limit.rb",
    """\
    # Rate limiting middleware.

    require_relative '../utils/helpers'
    require_relative '../exceptions'
    require_relative '../cache/base_cache'

    # Rate limiter using a cache backend.
    class RateLimiter
      def initialize(cache, limit: 100, window: 60)
        @cache = cache
        @limit = limit
        @window = window
      end

      def check(key)
        cache_key = "ratelimit:#{key}"
        current = @cache.get(cache_key)
        if current.nil?
          @cache.set(cache_key, 1, @window)
          return { allowed: true, remaining: @limit - 1 }
        end
        if current >= @limit
          get_logger('middleware.rate_limit').info("Rate limit exceeded: #{key}")
          return { allowed: false, remaining: 0 }
        end
        @cache.set(cache_key, current + 1, @window)
        { allowed: true, remaining: @limit - current - 1 }
      end
    end

    # Apply rate limiting to a request.
    def rate_limit_middleware(request, cache)
      validate_request(request)
      ip = request[:ip] || 'unknown'
      path = request[:path] || '/'
      limiter = RateLimiter.new(cache)
      result = limiter.check("#{ip}:#{path}")
      raise RateLimitError.new(retry_after: 60) unless result[:allowed]

      request[:rate_limit] = result
      request
    end
    """,
)

# ─── middleware/cors.rb ───
w(
    "middleware/cors.rb",
    """\
    # CORS middleware.

    require_relative '../utils/helpers'

    DEFAULT_ORIGINS = ['http://localhost:3000', 'https://app.example.com'].freeze

    # CORS policy configuration.
    class CorsPolicy
      attr_accessor :allowed_origins, :allowed_methods, :allow_credentials, :max_age

      def initialize(
        allowed_origins: DEFAULT_ORIGINS,
        allowed_methods: %w[GET POST PUT DELETE],
        allow_credentials: true,
        max_age: 86_400
      )
        @allowed_origins = allowed_origins
        @allowed_methods = allowed_methods
        @allow_credentials = allow_credentials
        @max_age = max_age
      end

      def origin_allowed?(origin)
        @allowed_origins.include?('*') || @allowed_origins.include?(origin)
      end

      def headers(origin)
        return {} unless origin_allowed?(origin)

        {
          'Access-Control-Allow-Origin' => origin,
          'Access-Control-Allow-Methods' => @allowed_methods.join(', '),
          'Access-Control-Max-Age' => @max_age.to_s
        }
      end
    end

    # Apply CORS headers to a request.
    def cors_middleware(request, policy = nil)
      validate_request(request)
      cors = policy || CorsPolicy.new
      origin = request[:origin] || ''
      if origin && !origin.empty?
        hdrs = cors.headers(origin)
        get_logger('middleware.cors').warn("CORS rejected: #{origin}") if hdrs.empty?
        return request.merge(cors_headers: hdrs)
      end
      request.merge(cors_headers: {})
    end
    """,
)

# ─── middleware/logging_middleware.rb ───
w(
    "middleware/logging_middleware.rb",
    """\
    # Request logging middleware.

    require_relative '../utils/helpers'

    SENSITIVE_FIELDS = %i[password token secret api_key].freeze

    # Log incoming requests.
    def logging_middleware(request)
      validate_request(request)
      request_id = request[:request_id] || generate_request_id
      safe = mask_sensitive(request, SENSITIVE_FIELDS)
      method = request[:method]
      path = request[:path]
      get_logger('middleware.logging').info("[#{request_id}] #{method} #{path}")
      request.merge(request_id: request_id, start_time: Time.now.to_f)
    end

    # Log response details.
    def log_response(request, status)
      request_id = request[:request_id]
      start_time = request[:start_time] || Time.now.to_f
      duration = ((Time.now.to_f - start_time) * 1000).to_i
      get_logger('middleware.logging').info("[#{request_id}] -> #{status} (#{duration}ms)")
    end
    """,
)

# ─── api/v1/auth.rb ───
w(
    "api/v1/auth.rb",
    """\
    # API v1 authentication endpoints.

    require_relative '../../utils/helpers'
    require_relative '../../validators/user'
    require_relative '../../services/auth_service'
    require_relative '../../database/connection'
    require_relative '../../events/dispatcher'
    require_relative '../../exceptions'

    module ApiV1Auth
      # Validate v1 auth request — name collision.
      def self.validate(request)
        validate_request(request)
        body = request[:body]
        raise ValidationError.new('Body required') unless body

        body[:email] = body[:username] if body[:username] && !body[:email]
        body
      end

      # Handle v1 login — entry point for deep call chain.
      def self.handle_login(request, db, events)
        get_logger('api.v1.auth').info('API v1 login')
        body = validate(request)
        login_data = UserValidator.validate_login(body)
        service = AuthenticationService.new(db, events)
        ip = request[:ip] || 'unknown'
        result = service.authenticate(login_data[:email], login_data[:password], ip)
        { status: 200, data: result }
      end

      # Handle v1 register.
      def self.handle_register(request, db, events)
        get_logger('api.v1.auth').info('API v1 register')
        body = validate(request)
        { status: 201, data: body }
      end
    end
    """,
)

# ─── api/v1/payments.rb ───
w(
    "api/v1/payments.rb",
    """\
    # API v1 payment endpoints.

    require_relative '../../utils/helpers'
    require_relative '../../validators/payment'
    require_relative '../../services/payment/processor'
    require_relative '../../database/connection'
    require_relative '../../events/dispatcher'

    module ApiV1Payments
      # Create payment.
      def self.handle_create_payment(request, db, events)
        validate_request(request)
        body = request[:body]
        payment_data = PaymentValidator.validate(body)
        processor = PaymentProcessor.new(db, events)
        result = processor.process_payment(
          payment_data[:user_id],
          payment_data[:amount],
          payment_data[:currency]
        )
        { status: 201, data: result }
      end

      # Refund payment.
      def self.handle_refund(request, db, events)
        validate_request(request)
        body = request[:body]
        processor = PaymentProcessor.new(db, events)
        result = processor.refund(body[:transaction_id], body[:reason] || '')
        { status: 200, data: result }
      end
    end
    """,
)

# ─── api/v1/users.rb ───
w(
    "api/v1/users.rb",
    """\
    # API v1 user endpoints.

    require_relative '../../utils/helpers'
    require_relative '../../validators/user'
    require_relative '../../database/connection'
    require_relative '../../database/queries'
    require_relative '../../exceptions'

    module ApiV1Users
      # Get user by ID.
      def self.handle_get_user(request, db)
        validate_request(request)
        params = request[:params] || {}
        user_id = params[:id] || ''
        get_logger('api.v1.users').info("Getting user: #{user_id}")
        user = db.find_by_id('users', user_id)
        raise NotFoundError.new('User', user_id) unless user

        { status: 200, data: user }
      end

      # List users.
      def self.handle_list_users(request, db)
        validate_request(request)
        params = request[:params] || {}
        page = (params[:page] || '1').to_i
        queries = UserQueries.new(db)
        users = queries.find_active_users(200)
        { status: 200, data: paginate(users, page: page) }
      end

      # Update user.
      def self.handle_update_user(request, db)
        validate_request(request)
        params = request[:params] || {}
        body = request[:body]
        validated = UserValidator.validate(body)
        db.update('users', params[:id] || '', validated)
        { status: 200, data: validated }
      end
    end
    """,
)

# ─── api/v2/auth.rb ───
w(
    "api/v2/auth.rb",
    """\
    # API v2 authentication endpoints — improved over v1.

    require_relative '../../utils/helpers'
    require_relative '../../validators/user'
    require_relative '../../services/auth_service'
    require_relative '../../database/connection'
    require_relative '../../events/dispatcher'
    require_relative '../../exceptions'
    require_relative '../../auth/tokens'

    module ApiV2Auth
      # Validate v2 auth request — name collision.
      def self.validate(request)
        validate_request(request)
        body = request[:body]
        raise ValidationError.new('Body required') unless body
        raise ValidationError.new('Email required', field: :email) unless body[:email]

        body
      end

      # Handle v2 login with device tracking.
      def self.handle_login(request, db, events)
        get_logger('api.v2.auth').info('API v2 login')
        body = validate(request)
        login_data = UserValidator.validate_login(body)
        service = AuthenticationService.new(db, events)
        ip = request[:ip] || 'unknown'
        result = service.authenticate(login_data[:email], login_data[:password], ip)
        { status: 200, data: result.merge(api_version: 'v2') }
      end

      # Handle v2 token refresh.
      def self.handle_token_refresh(request, db, events)
        get_logger('api.v2.auth').info('API v2 token refresh')
        validate_request(request)
        old_token = request[:token] || ''
        raise AuthenticationError.new('Refresh token required') if old_token.empty?

        service = AuthenticationService.new(db, events)
        user = service.verify_token(old_token)
        raise AuthenticationError.new('Invalid refresh token') unless user

        new_token = generate_token(user)
        { status: 200, data: { token: new_token, api_version: 'v2' } }
      end
    end
    """,
)

# ─── api/v2/payments.rb ───
w(
    "api/v2/payments.rb",
    """\
    # API v2 payment endpoints with webhook support.

    require_relative '../../utils/helpers'
    require_relative '../../validators/payment'
    require_relative '../../services/payment/processor'
    require_relative '../../database/connection'
    require_relative '../../events/dispatcher'

    module ApiV2Payments
      # Create payment with idempotency.
      def self.handle_create_payment(request, db, events)
        validate_request(request)
        body = request[:body]
        headers = request[:headers] || {}
        idempotency_key = headers['Idempotency-Key'] || ''
        get_logger('api.v2.payments').info("V2 create payment (idempotency=#{idempotency_key[0, 12]})")
        payment_data = PaymentValidator.validate(body)
        processor = PaymentProcessor.new(db, events)
        result = processor.process_payment(
          payment_data[:user_id],
          payment_data[:amount],
          payment_data[:currency]
        )
        { status: 201, data: result }
      end

      # Handle webhook.
      def self.handle_webhook(request, db, events)
        validate_request(request)
        body = request[:body]
        event_type = body[:type]
        get_logger('api.v2.payments').info("Webhook: #{event_type}")
        { status: 200, data: { acknowledged: true } }
      end

      # Revenue report.
      def self.handle_revenue_report(request, db, events)
        validate_request(request)
        processor = PaymentProcessor.new(db, events)
        { status: 200, data: {} }
      end
    end
    """,
)

# ─── models/payment.rb ───
w(
    "models/payment.rb",
    """\
    # Payment model.

    require_relative '../utils/helpers'

    # Payment statuses.
    module PaymentStatus
      PENDING = 'pending'
      PROCESSING = 'processing'
      COMPLETED = 'completed'
      FAILED = 'failed'
      REFUNDED = 'refunded'
    end

    # Payment record.
    class Payment
      attr_reader :id, :user_id, :amount, :currency, :transaction_id, :created_at
      attr_accessor :status, :completed_at

      def initialize(id:, user_id:, amount:, currency:, transaction_id:, status: PaymentStatus::PENDING, created_at: nil, completed_at: nil)
        @id = id
        @user_id = user_id
        @amount = amount
        @currency = currency
        @transaction_id = transaction_id
        @status = status
        @created_at = created_at || Time.now.to_i
        @completed_at = completed_at
      end

      def complete
        @status = PaymentStatus::COMPLETED
        @completed_at = Time.now.to_i
        get_logger('models.payment').info("Payment #{@transaction_id} completed")
      end

      def fail(reason)
        @status = PaymentStatus::FAILED
        get_logger('models.payment').info("Payment #{@transaction_id} failed: #{reason}")
      end

      def do_refund
        @status = PaymentStatus::REFUNDED
        get_logger('models.payment').info("Payment #{@transaction_id} refunded")
      end

      def completed?
        @status == PaymentStatus::COMPLETED
      end

      def to_h
        {
          id: @id,
          user_id: @user_id,
          amount: @amount,
          currency: @currency,
          transaction_id: @transaction_id,
          status: @status,
          created_at: @created_at,
          completed_at: @completed_at
        }
      end
    end
    """,
)

# ─── models/types.rb ───
w(
    "models/types.rb",
    """\
    # Shared type definitions as modules of constants.

    module UserRole
      USER = 'user'
      ADMIN = 'admin'
      MODERATOR = 'moderator'
    end

    module EventType
      USER_REGISTERED = 'user.registered'
      LOGIN_SUCCESS = 'auth.login_success'
      LOGIN_FAILED = 'auth.login_failed'
      PAYMENT_COMPLETED = 'payment.completed'
      PAYMENT_REFUNDED = 'payment.refunded'
      PASSWORD_CHANGED = 'auth.password_changed'
    end

    module NotificationChannel
      EMAIL = 'email'
      SMS = 'sms'
      PUSH = 'push'
      IN_APP = 'in_app'
    end
    """,
)

# ─── routes/payments.rb ───
w(
    "routes/payments.rb",
    """\
    # Payment routes.

    require_relative '../utils/helpers'
    require_relative '../services/payment/processor'
    require_relative '../database/connection'
    require_relative '../events/dispatcher'
    require_relative '../auth/tokens'

    # Create payment route.
    def create_payment_route(request, db, events)
      validate_request(request)
      body = request[:body]
      processor = PaymentProcessor.new(db, events)
      result = processor.process_payment(
        body[:user_id],
        body[:amount].to_f,
        body[:currency] || 'USD'
      )
      { status: 201, data: result }
    end

    # Refund route.
    def refund_route(request, db, events)
      validate_request(request)
      body = request[:body]
      processor = PaymentProcessor.new(db, events)
      result = processor.refund(body[:transaction_id])
      { status: 200, data: result }
    end
    """,
)

# ─── routes/users.rb ───
w(
    "routes/users.rb",
    """\
    # User routes.

    require_relative '../utils/helpers'
    require_relative '../database/connection'
    require_relative '../database/queries'
    require_relative '../validators/user'
    require_relative '../exceptions'

    # Get user route.
    def get_user_route(request, db)
      validate_request(request)
      params = request[:params] || {}
      user = db.find_by_id('users', params[:id] || '')
      raise NotFoundError.new('User', params[:id] || '') unless user

      { status: 200, data: user }
    end

    # List users route.
    def list_users_route_v2(request, db)
      validate_request(request)
      queries = UserQueries.new(db)
      users = queries.find_active_users(200)
      { status: 200, data: paginate(users) }
    end

    # Update user route.
    def update_user_route(request, db)
      validate_request(request)
      params = request[:params] || {}
      body = request[:body]
      validated = UserValidator.validate(body)
      db.update('users', params[:id] || '', validated)
      { status: 200, data: validated }
    end
    """,
)

# ─── routes/notifications.rb ───
w(
    "routes/notifications.rb",
    """\
    # Notification routes.

    require_relative '../utils/helpers'
    require_relative '../database/connection'
    require_relative '../services/notification/manager'

    # Send notification route.
    def send_notification_route(request, db)
      validate_request(request)
      body = request[:body]
      manager = NotificationManager.new(db)
      notification = manager.send_notification(
        body[:user_id],
        body[:channel] || 'email',
        body[:subject],
        body[:body]
      )
      { status: 201, data: notification }
    end
    """,
)

# ─── tasks/email_task.rb ───
w(
    "tasks/email_task.rb",
    """\
    # Email background tasks.

    require_relative '../utils/helpers'
    require_relative '../database/connection'
    require_relative '../services/email/sender'

    # Send welcome email.
    def send_welcome_email(user_data, db)
      get_logger('tasks.email').info("Sending welcome email to #{user_data[:email]}")
      sender = EmailSender.new(db)
      sender.send_template(user_data[:email], 'welcome', { 'name' => user_data[:name] })
    end

    # Send password reset email.
    def send_password_reset_email(email, reset_link, db)
      get_logger('tasks.email').info("Sending password reset to #{email}")
      sender = EmailSender.new(db)
      sender.send_template(email, 'password_reset', { 'link' => reset_link })
    end

    # Process email queue.
    def process_email_queue(db)
      get_logger('tasks.email').info('Processing email queue')
      sender = EmailSender.new(db)
      pending = db.find_all('notifications', { channel: 'email', status: 'pending' })
      sent = 0
      failed = 0
      pending.each do |n|
        begin
          sender.send_email(n[:user_id], n[:subject], n[:body])
          sent += 1
        rescue StandardError
          failed += 1
        end
      end
      { sent: sent, failed: failed }
    end
    """,
)

# ─── tasks/payment_task.rb ───
w(
    "tasks/payment_task.rb",
    """\
    # Payment background tasks.

    require_relative '../utils/helpers'
    require_relative '../database/connection'
    require_relative '../database/queries'
    require_relative '../services/payment/processor'
    require_relative '../events/dispatcher'

    # Process pending payments.
    def process_pending_payments(db, events)
      get_logger('tasks.payment').info('Processing pending payments')
      queries = PaymentQueries.new(db)
      pending = queries.find_user_payments('', 'pending')
      processed = 0
      failed = 0
      pending.each do |payment|
        begin
          queries.update_status(payment[:transaction_id], 'completed')
          processed += 1
        rescue StandardError
          queries.update_status(payment[:transaction_id], 'failed')
          failed += 1
        end
      end
      { processed: processed, failed: failed }
    end

    # Reconcile payments.
    def reconcile_payments(db, events)
      get_logger('tasks.payment').info('Reconciling payments')
      queries = PaymentQueries.new(db)
      processing = queries.find_user_payments('', 'processing')
      resolved = 0
      processing.each do |payment|
        queries.update_status(payment[:transaction_id], 'completed')
        resolved += 1
      end
      { resolved: resolved }
    end
    """,
)

# ─── tasks/cleanup_task.rb ───
w(
    "tasks/cleanup_task.rb",
    """\
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
    """,
)

# ─── config_extended.rb (new config for extended settings) ───
w(
    "config_extended.rb",
    """\
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
    """,
)

# ─── app_extended.rb (new app entry point using all services) ───
w(
    "app_extended.rb",
    """\
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
    """,
)

print("\nRuby fixture generation complete")
