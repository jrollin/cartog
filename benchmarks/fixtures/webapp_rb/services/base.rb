# Service base class for the services layer.

require_relative '../utils/helpers'
require_relative '../database/connection'

# Re-export BaseService from auth/service for service-layer use.
# BaseService is defined in auth/service.rb. This file provides the
# Auditable module for mixin-based audit logging.

# Auditable mixin for services that need audit trails.
module Auditable
  def record_audit(action, actor, resource, details = {})
    @audit_log ||= []
    @audit_log << {
      action: action,
      actor: actor,
      resource: resource,
      details: details,
      timestamp: Time.now.to_i
    }
    get_logger('auditable').info("Audit: #{actor} #{action} on #{resource}")
  end

  def get_audit_trail(resource = nil, limit = 50)
    @audit_log ||= []
    entries = @audit_log
    entries = entries.select { |e| e[:resource] == resource } if resource
    entries.last(limit).reverse
  end
end
