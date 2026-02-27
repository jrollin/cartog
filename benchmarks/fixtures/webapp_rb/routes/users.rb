# User routes.

require_relative '../utils/helpers'
require_relative '../database/connection'
require_relative '../database/queries'
require_relative '../validators/user'
require_relative '../exceptions'

# Get user route.
def get_user_route(request, db)
  validate_request(request)
  params = request[:params] || {}
  user = db.find_by_id('users', params[:id] || '')
  raise NotFoundError.new('User', params[:id] || '') unless user

  { status: 200, data: user }
end

# List users route.
def list_users_route_v2(request, db)
  validate_request(request)
  queries = UserQueries.new(db)
  users = queries.find_active_users(200)
  { status: 200, data: paginate(users) }
end

# Update user route.
def update_user_route(request, db)
  validate_request(request)
  params = request[:params] || {}
  body = request[:body]
  validated = UserValidator.validate(body)
  db.update('users', params[:id] || '', validated)
  { status: 200, data: validated }
end
