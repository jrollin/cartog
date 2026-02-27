# Payment background tasks.

require_relative '../utils/helpers'
require_relative '../database/connection'
require_relative '../database/queries'
require_relative '../services/payment/processor'
require_relative '../events/dispatcher'

# Process pending payments.
def process_pending_payments(db, events)
  get_logger('tasks.payment').info('Processing pending payments')
  queries = PaymentQueries.new(db)
  pending = queries.find_user_payments('', 'pending')
  processed = 0
  failed = 0
  pending.each do |payment|
    begin
      queries.update_status(payment[:transaction_id], 'completed')
      processed += 1
    rescue StandardError
      queries.update_status(payment[:transaction_id], 'failed')
      failed += 1
    end
  end
  { processed: processed, failed: failed }
end

# Reconcile payments.
def reconcile_payments(db, events)
  get_logger('tasks.payment').info('Reconciling payments')
  queries = PaymentQueries.new(db)
  processing = queries.find_user_payments('', 'processing')
  resolved = 0
  processing.each do |payment|
    queries.update_status(payment[:transaction_id], 'completed')
    resolved += 1
  end
  { resolved: resolved }
end
