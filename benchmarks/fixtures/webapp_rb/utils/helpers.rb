# Shared utility helpers.

require_relative 'logging'

# Get a named logger instance.
def get_logger(name)
  Logging.get_logger(name)
end

# Validate that a request hash has required fields.
def validate_request(request)
  raise ArgumentError, 'Request must be a Hash' unless request.is_a?(Hash)

  %i[method path].each do |field|
    raise ArgumentError, "Missing required field: #{field}" unless request.key?(field)
  end
  true
end

# Generate a unique request identifier.
def generate_request_id
  ts = (Time.now.to_f * 1000).to_i
  rand_part = SecureRandom.hex(4)
  "req-#{ts}-#{rand_part}"
end

# Sanitize user input by removing control characters.
def sanitize_input(value)
  return '' if value.nil? || value.empty?

  value.gsub(/[\x00-\x1f]/, '').strip
end

# Paginate a list of items.
def paginate(items, page: 1, per_page: 20)
  total = items.length
  start_idx = (page - 1) * per_page
  page_items = items[start_idx, per_page] || []
  {
    items: page_items,
    page: page,
    per_page: per_page,
    total: total,
    pages: (total.to_f / per_page).ceil
  }
end

# Mask sensitive fields in a hash for logging.
def mask_sensitive(data, fields)
  masked = data.dup
  fields.each do |field|
    if masked.key?(field)
      val = masked[field].to_s
      masked[field] = val.length > 4 ? "#{val[0, 2]}***#{val[-2, 2]}" : '***'
    end
  end
  masked
end

# Retry an operation with exponential backoff.
def retry_operation(max_retries: 3, delay: 1.0)
  last_error = nil
  max_retries.times do |attempt|
    begin
      return yield
    rescue StandardError => e
      last_error = e
      sleep(delay * (2**attempt))
    end
  end
  raise last_error
end
