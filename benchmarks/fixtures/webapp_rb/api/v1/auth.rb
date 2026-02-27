# API v1 authentication endpoints.

require_relative '../../utils/helpers'
require_relative '../../validators/user'
require_relative '../../services/auth_service'
require_relative '../../database/connection'
require_relative '../../events/dispatcher'
require_relative '../../exceptions'

module ApiV1Auth
  # Validate v1 auth request — name collision.
  def self.validate(request)
    validate_request(request)
    body = request[:body]
    raise ValidationError.new('Body required') unless body

    body[:email] = body[:username] if body[:username] && !body[:email]
    body
  end

  # Handle v1 login — entry point for deep call chain.
  def self.handle_login(request, db, events)
    get_logger('api.v1.auth').info('API v1 login')
    body = validate(request)
    login_data = UserValidator.validate_login(body)
    service = AuthenticationService.new(db, events)
    ip = request[:ip] || 'unknown'
    result = service.authenticate(login_data[:email], login_data[:password], ip)
    { status: 200, data: result }
  end

  # Handle v1 register.
  def self.handle_register(request, db, events)
    get_logger('api.v1.auth').info('API v1 register')
    body = validate(request)
    { status: 201, data: body }
  end
end
