# API v1 user endpoints.

require_relative '../../utils/helpers'
require_relative '../../validators/user'
require_relative '../../database/connection'
require_relative '../../database/queries'
require_relative '../../exceptions'

module ApiV1Users
  # Get user by ID.
  def self.handle_get_user(request, db)
    validate_request(request)
    params = request[:params] || {}
    user_id = params[:id] || ''
    get_logger('api.v1.users').info("Getting user: #{user_id}")
    user = db.find_by_id('users', user_id)
    raise NotFoundError.new('User', user_id) unless user

    { status: 200, data: user }
  end

  # List users.
  def self.handle_list_users(request, db)
    validate_request(request)
    params = request[:params] || {}
    page = (params[:page] || '1').to_i
    queries = UserQueries.new(db)
    users = queries.find_active_users(200)
    { status: 200, data: paginate(users, page: page) }
  end

  # Update user.
  def self.handle_update_user(request, db)
    validate_request(request)
    params = request[:params] || {}
    body = request[:body]
    validated = UserValidator.validate(body)
    db.update('users', params[:id] || '', validated)
    { status: 200, data: validated }
  end
end
