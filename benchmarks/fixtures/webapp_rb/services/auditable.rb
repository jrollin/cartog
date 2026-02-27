# Auditable service with standalone audit trail.

require_relative '../utils/helpers'
require_relative '../auth/service'
require_relative 'base'

# Service with audit logging support.
class AuditableService < BaseService
  include Auditable

  def initialize
    super
    @audit_log = []
  end
end
