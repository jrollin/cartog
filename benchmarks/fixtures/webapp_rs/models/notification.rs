use crate::utils::helpers::get_logger;

/// Delivery channels for notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum NotificationChannel {
    /// Email delivery.
    Email,
    /// SMS delivery.
    Sms,
    /// Push notification.
    Push,
    /// In-app notification.
    InApp,
}

/// A notification record in the system.
pub struct Notification {
    /// Unique notification identifier.
    pub id: u64,
    /// The target user ID.
    pub user_id: u64,
    /// The delivery channel.
    pub channel: NotificationChannel,
    /// The notification subject.
    pub subject: String,
    /// The notification body.
    pub body: String,
    /// Whether the notification has been read.
    pub read: bool,
}

impl Notification {
    /// Create a new unread notification.
    pub fn new(id: u64, user_id: u64, channel: NotificationChannel, subject: &str, body: &str) -> Self {
        let logger = get_logger("models.notification");
        logger.info(&format!("Creating notification {} for user {}", id, user_id));
        Self {
            id,
            user_id,
            channel,
            subject: subject.to_string(),
            body: body.to_string(),
            read: false,
        }
    }

    /// Mark the notification as read.
    pub fn mark_read(&mut self) {
        let logger = get_logger("models.notification");
        self.read = true;
        logger.info(&format!("Notification {} marked as read", self.id));
    }

    /// Find notifications for a user (simulated).
    pub fn find_by_user(user_id: u64) -> Vec<Notification> {
        let logger = get_logger("models.notification");
        logger.info(&format!("Looking up notifications for user {}", user_id));
        Vec::new()
    }
}
