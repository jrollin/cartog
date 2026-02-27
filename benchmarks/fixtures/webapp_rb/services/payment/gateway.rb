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
