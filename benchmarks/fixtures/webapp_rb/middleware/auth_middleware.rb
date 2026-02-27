# Auth middleware (services layer).

require_relative '../utils/helpers'
require_relative '../auth/tokens'
require_relative '../auth/middleware'
require_relative '../exceptions'

# Authentication middleware for the services layer.
class AuthMiddleware
  PUBLIC_PATHS = %w[/health /login /register].freeze

  def initialize(app)
    @app = app
  end

  def call(request)
    validate_request(request)
    return @app.call(request) if PUBLIC_PATHS.include?(request[:path])

    token = extract_token(request)
    raise AuthenticationError.new('Missing token') unless token

    begin
      user = validate_token(token)
      request[:user] = user
      request[:authenticated] = true
      @app.call(request)
    rescue TokenError
      get_logger('middleware.auth').warn('Token validation failed')
      raise AuthenticationError.new('Invalid token')
    end
  end
end
