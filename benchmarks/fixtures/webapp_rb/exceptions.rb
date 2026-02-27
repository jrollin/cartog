# Application error hierarchy.

# Base application error.
class AppError < StandardError
  attr_reader :code

  def initialize(message = 'Application error', code: 500)
    @code = code
    super(message)
  end

  def to_h
    { error: self.class.name, message: message, code: @code }
  end
end

# Raised when input validation fails.
class ValidationError < AppError
  attr_reader :field

  def initialize(message = 'Validation failed', field: nil)
    @field = field
    super(message, code: 400)
  end
end

# Raised when a payment operation fails.
class PaymentError < AppError
  attr_reader :transaction_id

  def initialize(message = 'Payment failed', transaction_id: nil)
    @transaction_id = transaction_id
    super(message, code: 402)
  end
end

# Raised when a resource is not found.
class NotFoundError < AppError
  attr_reader :resource, :identifier

  def initialize(resource, identifier)
    @resource = resource
    @identifier = identifier
    super("#{resource} with id '#{identifier}' not found", code: 404)
  end
end

# Raised when rate limit is exceeded.
class RateLimitError < AppError
  attr_reader :retry_after

  def initialize(retry_after: 60)
    @retry_after = retry_after
    super("Rate limit exceeded. Retry after #{retry_after}s", code: 429)
  end
end

# Raised when authentication fails.
class AuthenticationError < AppError
  def initialize(message = 'Authentication required')
    super(message, code: 401)
  end
end

# Raised when authorization fails.
class AuthorizationError < AppError
  def initialize(action, resource)
    super("Not authorized to #{action} on #{resource}", code: 403)
  end
end

# Raised when a database operation fails.
class DatabaseError < AppError
  attr_reader :query

  def initialize(message = 'Database error', query: nil)
    @query = query
    super(message, code: 500)
  end
end
