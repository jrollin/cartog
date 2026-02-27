# Email background tasks.

require_relative '../utils/helpers'
require_relative '../database/connection'
require_relative '../services/email/sender'

# Send welcome email.
def send_welcome_email(user_data, db)
  get_logger('tasks.email').info("Sending welcome email to #{user_data[:email]}")
  sender = EmailSender.new(db)
  sender.send_template(user_data[:email], 'welcome', { 'name' => user_data[:name] })
end

# Send password reset email.
def send_password_reset_email(email, reset_link, db)
  get_logger('tasks.email').info("Sending password reset to #{email}")
  sender = EmailSender.new(db)
  sender.send_template(email, 'password_reset', { 'link' => reset_link })
end

# Process email queue.
def process_email_queue(db)
  get_logger('tasks.email').info('Processing email queue')
  sender = EmailSender.new(db)
  pending = db.find_all('notifications', { channel: 'email', status: 'pending' })
  sent = 0
  failed = 0
  pending.each do |n|
    begin
      sender.send_email(n[:user_id], n[:subject], n[:body])
      sent += 1
    rescue StandardError
      failed += 1
    end
  end
  { sent: sent, failed: failed }
end
