# API v2 authentication endpoints — improved over v1.

require_relative '../../utils/helpers'
require_relative '../../validators/user'
require_relative '../../services/auth_service'
require_relative '../../database/connection'
require_relative '../../events/dispatcher'
require_relative '../../exceptions'
require_relative '../../auth/tokens'

module ApiV2Auth
  # Validate v2 auth request — name collision.
  def self.validate(request)
    validate_request(request)
    body = request[:body]
    raise ValidationError.new('Body required') unless body
    raise ValidationError.new('Email required', field: :email) unless body[:email]

    body
  end

  # Handle v2 login with device tracking.
  def self.handle_login(request, db, events)
    get_logger('api.v2.auth').info('API v2 login')
    body = validate(request)
    login_data = UserValidator.validate_login(body)
    service = AuthenticationService.new(db, events)
    ip = request[:ip] || 'unknown'
    result = service.authenticate(login_data[:email], login_data[:password], ip)
    { status: 200, data: result.merge(api_version: 'v2') }
  end

  # Handle v2 token refresh.
  def self.handle_token_refresh(request, db, events)
    get_logger('api.v2.auth').info('API v2 token refresh')
    validate_request(request)
    old_token = request[:token] || ''
    raise AuthenticationError.new('Refresh token required') if old_token.empty?

    service = AuthenticationService.new(db, events)
    user = service.verify_token(old_token)
    raise AuthenticationError.new('Invalid refresh token') unless user

    new_token = generate_token(user)
    { status: 200, data: { token: new_token, api_version: 'v2' } }
  end
end
