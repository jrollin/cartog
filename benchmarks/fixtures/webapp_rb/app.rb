# Main application entry point.

require_relative 'config'
require_relative 'routes/auth'
require_relative 'routes/admin'
require_relative 'utils/logging'

# Create and configure the application.
def create_app
  config = Config.load
  app = App.new(config)
  register_routes(app)
  Logging.get_logger('app').info('Application created')
  app
end

# Register all route handlers.
def register_routes(app)
  app.route('/login', method(:login_route))
  app.route('/logout', method(:logout_route))
  app.route('/refresh', method(:refresh_route))
  app.route('/admin/impersonate', method(:impersonate_route))
  app.route('/admin/users', method(:list_users_route))
end

class App
  # Simple application container.

  def initialize(config)
    @config = config
    @routes = {}
  end

  def route(path, handler)
    @routes[path] = handler
  end

  def handle_request(path, request)
    handler = @routes[path]
    raise ArgumentError, "No route for #{path}" unless handler

    handler.call(request)
  end
end
