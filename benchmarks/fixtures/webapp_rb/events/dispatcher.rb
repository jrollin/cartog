# Event dispatcher.

require_relative '../utils/helpers'

# Central event bus.
class EventDispatcher
  def initialize
    @handlers = {}
    @event_log = []
  end

  # Register a handler for an event type.
  def on(event_type, &handler)
    @handlers[event_type] ||= []
    @handlers[event_type] << handler
    get_logger('events.dispatcher').info("Handler registered for: #{event_type}")
  end

  # Emit an event to all registered handlers.
  def emit(event_type, data = {})
    event = { type: event_type, data: data, timestamp: Time.now.to_i, processed: false }
    @event_log << event
    handlers = @handlers[event_type] || []
    get_logger('events.dispatcher').info("Emitting #{event_type} to #{handlers.length} handlers")
    invoked = 0
    handlers.each do |handler|
      begin
        handler.call(event)
        invoked += 1
      rescue StandardError => e
        get_logger('events.dispatcher').error("Handler error for #{event_type}: #{e}")
      end
    end
    event[:processed] = true
    invoked
  end

  # Get total event count.
  def event_count
    @event_log.length
  end
end
