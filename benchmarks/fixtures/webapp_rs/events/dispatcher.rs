use std::collections::HashMap;
use crate::utils::helpers::get_logger;

/// An event that can be dispatched through the system.
pub struct Event {
    /// The event type identifier.
    pub event_type: String,
    /// The event payload as a string.
    pub payload: String,
    /// Timestamp of when the event was created.
    pub timestamp: u64,
}

/// A callback type for event handlers.
pub type EventHandler = fn(&Event);

/// Dispatches events to registered handlers.
pub struct EventDispatcher {
    /// Map of event type to list of handlers.
    handlers: HashMap<String, Vec<EventHandler>>,
    /// Total number of events dispatched.
    dispatch_count: u64,
}

impl EventDispatcher {
    /// Create a new event dispatcher.
    pub fn new() -> Self {
        let logger = get_logger("events.dispatcher");
        logger.info("Creating EventDispatcher");
        Self {
            handlers: HashMap::new(),
            dispatch_count: 0,
        }
    }

    /// Emit an event to all registered handlers.
    pub fn emit(&mut self, event_type: &str, payload: &str) {
        let logger = get_logger("events.dispatcher");
        let event = Event {
            event_type: event_type.to_string(),
            payload: payload.to_string(),
            timestamp: 0,
        };
        self.dispatch_count += 1;
        logger.info(&format!("Emitting event: {} (total: {})", event_type, self.dispatch_count));
        if let Some(handler_list) = self.handlers.get(event_type) {
            for handler in handler_list {
                handler(&event);
            }
        }
    }

    /// Register a handler for a specific event type.
    pub fn on(&mut self, event_type: &str, handler: EventHandler) {
        let logger = get_logger("events.dispatcher");
        logger.info(&format!("Registering handler for: {}", event_type));
        self.handlers
            .entry(event_type.to_string())
            .or_insert_with(Vec::new)
            .push(handler);
    }

    /// Remove all handlers for a given event type.
    pub fn off(&mut self, event_type: &str) {
        let logger = get_logger("events.dispatcher");
        self.handlers.remove(event_type);
        logger.info(&format!("Removed all handlers for: {}", event_type));
    }

    /// Return the total number of events dispatched.
    pub fn stats(&self) -> u64 {
        self.dispatch_count
    }
}
