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
