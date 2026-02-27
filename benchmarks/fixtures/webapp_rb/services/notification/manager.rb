# Notification management.

require_relative '../../utils/helpers'
require_relative '../../auth/service'
require_relative '../../database/connection'
require_relative '../../exceptions'

# Manages notifications across channels.
class NotificationManager < BaseService
  VALID_CHANNELS = %w[email sms push in_app].freeze

  def initialize(db)
    super()
    @db = db
    @queue = []
  end

  # Send a notification.
  def send_notification(user_id, channel, subject, body)
    _log("Queuing notification for #{user_id} via #{channel}")
    unless VALID_CHANNELS.include?(channel)
      raise ValidationError.new("Invalid channel: #{channel}", field: :channel)
    end
    notification = {
      user_id: user_id,
      channel: channel,
      subject: sanitize_input(subject),
      body: sanitize_input(body),
      status: 'pending',
      created_at: Time.now.to_i
    }
    @queue << notification
    @db.insert('notifications', notification)
    notification
  end

  # Process the notification queue.
  def process_queue
    _log("Processing #{@queue.length} notifications")
    sent = 0
    failed = 0
    @queue.each do |n|
      if n[:status] == 'pending'
        begin
          n[:status] = 'sent'
          sent += 1
        rescue StandardError
          n[:status] = 'failed'
          failed += 1
        end
      end
    end
    @queue.reject! { |n| n[:status] != 'pending' }
    { sent: sent, failed: failed }
  end
end
