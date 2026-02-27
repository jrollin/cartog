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
