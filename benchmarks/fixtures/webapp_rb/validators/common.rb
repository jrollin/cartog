# Common validation utilities.

require_relative '../utils/helpers'
require_relative '../exceptions'

EMAIL_REGEX = /\A[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}\z/

# Validate email format.
def validate_email(email)
  raise ValidationError.new('Email is required', field: :email) unless email && !email.empty?

  clean = email.strip.downcase
  unless EMAIL_REGEX.match?(clean)
    raise ValidationError.new("Invalid email: #{email}", field: :email)
  end
  clean
end

# Validate string length.
def validate_string(value, field, min_len: 1, max_len: 255)
  raise ValidationError.new("#{field} is required", field: field) unless value && !value.empty?

  stripped = value.strip
  raise ValidationError.new("#{field} too short", field: field) if stripped.length < min_len
  raise ValidationError.new("#{field} too long", field: field) if stripped.length > max_len

  stripped
end

# Validate positive number.
def validate_positive_number(value, field)
  num = value.to_f
  raise ValidationError.new("#{field} must be a number", field: field) if num.zero? && value.to_s != '0'
  raise ValidationError.new("#{field} must be positive", field: field) if num <= 0

  num
end

# Validate enum value.
def validate_enum(value, allowed, field)
  unless allowed.include?(value)
    raise ValidationError.new("Invalid #{field}: '#{value}'. Allowed: #{allowed.join(', ')}", field: field)
  end
  value
end
