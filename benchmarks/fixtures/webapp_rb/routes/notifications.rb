# Notification routes.

require_relative '../utils/helpers'
require_relative '../database/connection'
require_relative '../services/notification/manager'

# Send notification route.
def send_notification_route(request, db)
  validate_request(request)
  body = request[:body]
  manager = NotificationManager.new(db)
  notification = manager.send_notification(
    body[:user_id],
    body[:channel] || 'email',
    body[:subject],
    body[:body]
  )
  { status: 201, data: notification }
end
