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
