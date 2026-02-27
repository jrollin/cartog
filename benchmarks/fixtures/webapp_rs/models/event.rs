use crate::utils::helpers::get_logger;

/// Types of events in the system.
#[derive(Debug, Clone, PartialEq)]
pub enum EventType {
    /// A user was registered.
    UserRegistered,
    /// A login attempt succeeded.
    LoginSuccess,
    /// A login attempt failed.
    LoginFailed,
    /// A payment was completed.
    PaymentCompleted,
    /// A payment was refunded.
    PaymentRefunded,
    /// A password was changed.
    PasswordChanged,
}

/// A persisted event record.
pub struct EventRecord {
    /// Unique event identifier.
    pub id: u64,
    /// The type of event.
    pub event_type: EventType,
    /// The serialized event payload.
    pub payload: String,
    /// The actor who triggered the event.
    pub actor_id: Option<u64>,
    /// Timestamp of the event.
    pub created_at: u64,
}

impl EventRecord {
    /// Create a new event record.
    pub fn new(id: u64, event_type: EventType, payload: &str, actor_id: Option<u64>) -> Self {
        let logger = get_logger("models.event");
        logger.info(&format!("Creating event: {:?}", event_type));
        Self {
            id,
            event_type,
            payload: payload.to_string(),
            actor_id,
            created_at: 0,
        }
    }

    /// Find events by type (simulated).
    pub fn find_by_type(event_type: &EventType) -> Vec<EventRecord> {
        let logger = get_logger("models.event");
        logger.info(&format!("Looking up events of type: {:?}", event_type));
        Vec::new()
    }

    /// Find events by actor (simulated).
    pub fn find_by_actor(actor_id: u64) -> Vec<EventRecord> {
        let logger = get_logger("models.event");
        logger.info(&format!("Looking up events for actor: {}", actor_id));
        Vec::new()
    }
}
