# Authentication routes.

require_relative '../auth/service'
require_relative '../auth/middleware'
require_relative '../auth/tokens'
require_relative '../utils/logging'

# Handle login requests.
def login_route(request)
  config = Config.load
  service = AuthService.new(request[:db])
  token = service.login(request[:email], request[:password])
  if token
    { token: token, status: 200 }
  else
    { error: 'Invalid credentials', status: 401 }
  end
end

# Handle logout requests.
def logout_route(request)
  auth_required(request) do |req|
    service = AuthService.new(req[:db])
    service.logout(extract_token(req))
    { status: 200 }
  end
end

# Handle token refresh requests.
def refresh_route(request)
  token = extract_token(request)
  return { error: 'Missing token', status: 401 } unless token

  begin
    new_token = refresh_token(token)
    { token: new_token, status: 200 }
  rescue TokenError => e
    { error: e.message, status: 401 }
  end
end
