# Admin routes.

require_relative '../auth/service'
require_relative '../auth/middleware'
require_relative '../utils/logging'

# Handle admin impersonation requests.
def impersonate_route(request)
  admin_required(request) do |req|
    service = AdminService.new(req[:db])
    token = extract_token(req)
    target_id = req[:user_id]
    new_token = service.impersonate(token, target_id)
    { token: new_token, status: 200 }
  end
end

# Handle list all users requests.
def list_users_route(request)
  admin_required(request) do |req|
    service = AdminService.new(req[:db])
    token = extract_token(req)
    users = service.list_all_users(token)
    { users: users, status: 200 }
  end
end
