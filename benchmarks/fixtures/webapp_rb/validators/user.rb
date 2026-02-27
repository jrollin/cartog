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
