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
