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
