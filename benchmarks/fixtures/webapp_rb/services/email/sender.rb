# Email sending service.

require_relative '../../utils/helpers'
require_relative '../cacheable'
require_relative '../../auth/service'
require_relative '../../database/connection'
require_relative '../../exceptions'

TEMPLATES = {
  'welcome' => 'Welcome to our platform, {name}!',
  'password_reset' => 'Reset your password: {link}',
  'payment_receipt' => 'Payment of {amount} {currency} received. Txn: {txn_id}',
}.freeze

# Email sender with template support.
class EmailSender < BaseService
  include Cacheable

  def initialize(db)
    super()
    @db = db
    @sent_count = 0
    @failed_count = 0
  end

  # Send a single email.
  def send_email(to, subject, body)
    _log("Sending email to #{to}: #{subject}")
    raise ValidationError.new('Invalid email', field: :to) unless to.include?('@')

    begin
      @db.insert('notifications', { user_id: 'system', channel: 'email', subject: subject, body: body, status: 'sent' })
      @sent_count += 1
      true
    rescue StandardError => e
      get_logger('services.email').error("Email failed: #{e}")
      @failed_count += 1
      false
    end
  end

  # Send using a template.
  def send_template(to, template_name, context)
    template = TEMPLATES[template_name]
    raise ValidationError.new("Unknown template: #{template_name}", field: :template) unless template

    body = template.dup
    context.each { |key, val| body.gsub!("{#{key}}", val.to_s) }
    subject = "[App] #{template_name.tr('_', ' ')}"
    send_email(to, subject, body)
  end

  # Get sending statistics.
  def stats
    { sent: @sent_count, failed: @failed_count }
  end
end
