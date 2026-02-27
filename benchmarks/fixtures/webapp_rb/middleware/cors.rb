# CORS middleware.

require_relative '../utils/helpers'

DEFAULT_ORIGINS = ['http://localhost:3000', 'https://app.example.com'].freeze

# CORS policy configuration.
class CorsPolicy
  attr_accessor :allowed_origins, :allowed_methods, :allow_credentials, :max_age

  def initialize(
    allowed_origins: DEFAULT_ORIGINS,
    allowed_methods: %w[GET POST PUT DELETE],
    allow_credentials: true,
    max_age: 86_400
  )
    @allowed_origins = allowed_origins
    @allowed_methods = allowed_methods
    @allow_credentials = allow_credentials
    @max_age = max_age
  end

  def origin_allowed?(origin)
    @allowed_origins.include?('*') || @allowed_origins.include?(origin)
  end

  def headers(origin)
    return {} unless origin_allowed?(origin)

    {
      'Access-Control-Allow-Origin' => origin,
      'Access-Control-Allow-Methods' => @allowed_methods.join(', '),
      'Access-Control-Max-Age' => @max_age.to_s
    }
  end
end

# Apply CORS headers to a request.
def cors_middleware(request, policy = nil)
  validate_request(request)
  cors = policy || CorsPolicy.new
  origin = request[:origin] || ''
  if origin && !origin.empty?
    hdrs = cors.headers(origin)
    get_logger('middleware.cors').warn("CORS rejected: #{origin}") if hdrs.empty?
    return request.merge(cors_headers: hdrs)
  end
  request.merge(cors_headers: {})
end
