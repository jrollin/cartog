# Request logging middleware.

require_relative '../utils/helpers'

SENSITIVE_FIELDS = %i[password token secret api_key].freeze

# Log incoming requests.
def logging_middleware(request)
  validate_request(request)
  request_id = request[:request_id] || generate_request_id
  safe = mask_sensitive(request, SENSITIVE_FIELDS)
  method = request[:method]
  path = request[:path]
  get_logger('middleware.logging').info("[#{request_id}] #{method} #{path}")
  request.merge(request_id: request_id, start_time: Time.now.to_f)
end

# Log response details.
def log_response(request, status)
  request_id = request[:request_id]
  start_time = request[:start_time] || Time.now.to_f
  duration = ((Time.now.to_f - start_time) * 1000).to_i
  get_logger('middleware.logging').info("[#{request_id}] -> #{status} (#{duration}ms)")
end
