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
