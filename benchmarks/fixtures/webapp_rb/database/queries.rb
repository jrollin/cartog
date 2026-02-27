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
