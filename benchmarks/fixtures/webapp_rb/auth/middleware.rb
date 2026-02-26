# Authentication middleware.

require_relative 'tokens'
require_relative '../models/user'
require_relative '../utils/logging'

# Middleware that requires a valid authentication token.
def auth_required(request)
  token = extract_token(request)
  return { error: 'Missing authentication token', status: 401 } if token.nil?

  begin
    user = validate_token(token)
  rescue ExpiredTokenError
    Logging.get_logger('middleware').warn('Expired token used')
    return { error: 'Token expired', status: 401 }
  rescue TokenError => e
    Logging.get_logger('middleware').warn("Invalid token: #{e.message}")
    return { error: 'Invalid token', status: 401 }
  end

  request[:user] = user
  yield request
end

# Middleware that requires admin privileges.
def admin_required(request)
  auth_required(request) do |req|
    user = req[:user]
    return { error: 'Admin access required', status: 403 } unless user && user.is_admin

    yield req
  end
end

# Extract the bearer token from a request.
def extract_token(request)
  auth_header = request.dig(:headers, 'Authorization') || ''
  return auth_header[7..] if auth_header.start_with?('Bearer ')

  nil
end

# Get the current authenticated user from the request.
def get_current_user(request)
  token = extract_token(request)
  return nil unless token

  begin
    validate_token(token)
  rescue TokenError
    nil
  end
end
