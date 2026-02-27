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
