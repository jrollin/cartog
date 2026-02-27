# Rate limiting middleware.

require_relative '../utils/helpers'
require_relative '../exceptions'
require_relative '../cache/base_cache'

# Rate limiter using a cache backend.
class RateLimiter
  def initialize(cache, limit: 100, window: 60)
    @cache = cache
    @limit = limit
    @window = window
  end

  def check(key)
    cache_key = "ratelimit:#{key}"
    current = @cache.get(cache_key)
    if current.nil?
      @cache.set(cache_key, 1, @window)
      return { allowed: true, remaining: @limit - 1 }
    end
    if current >= @limit
      get_logger('middleware.rate_limit').info("Rate limit exceeded: #{key}")
      return { allowed: false, remaining: 0 }
    end
    @cache.set(cache_key, current + 1, @window)
    { allowed: true, remaining: @limit - current - 1 }
  end
end

# Apply rate limiting to a request.
def rate_limit_middleware(request, cache)
  validate_request(request)
  ip = request[:ip] || 'unknown'
  path = request[:path] || '/'
  limiter = RateLimiter.new(cache)
  result = limiter.check("#{ip}:#{path}")
  raise RateLimitError.new(retry_after: 60) unless result[:allowed]

  request[:rate_limit] = result
  request
end
