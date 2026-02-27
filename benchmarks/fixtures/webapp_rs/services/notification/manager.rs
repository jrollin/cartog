use crate::utils::helpers::{get_logger, sanitize_input};
use crate::services::base::{Service, ServiceHealth, BaseServiceImpl};

/// A notification to be delivered to a user.
pub struct Notification {
    /// The target user ID.
    pub user_id: String,
    /// The delivery channel (email, sms, push, in_app).
    pub channel: String,
    /// The notification subject.
    pub subject: String,
    /// The notification body.
    pub body: String,
    /// Current delivery status.
    pub status: String,
}

/// Valid notification channels.
const VALID_CHANNELS: &[&str] = &["email", "sms", "push", "in_app"];

/// Manages notification creation and delivery.
pub struct NotificationManager {
    /// The underlying service state.
    inner: BaseServiceImpl,
    /// Queue of pending notifications.
    queue: Vec<Notification>,
}

impl NotificationManager {
    /// Create a new notification manager.
    pub fn new() -> Self {
        let logger = get_logger("services.notification");
        logger.info("Creating NotificationManager");
        Self {
            inner: BaseServiceImpl::new("notification_manager"),
            queue: Vec::new(),
        }
    }

    /// Queue a notification for delivery.
    pub fn send(
        &mut self,
        user_id: &str,
        channel: &str,
        subject: &str,
        body: &str,
    ) -> Result<&Notification, String> {
        let logger = get_logger("services.notification");
        self.inner.require_initialized()?;
        if !VALID_CHANNELS.contains(&channel) {
            return Err(format!("Invalid channel: {}", channel));
        }
        logger.info(&format!("Queuing notification for {} via {}", user_id, channel));
        self.queue.push(Notification {
            user_id: user_id.to_string(),
            channel: channel.to_string(),
            subject: sanitize_input(subject),
            body: sanitize_input(body),
            status: "pending".to_string(),
        });
        Ok(self.queue.last().unwrap())
    }

    /// Process all pending notifications in the queue.
    pub fn process_queue(&mut self) -> (usize, usize) {
        let logger = get_logger("services.notification");
        logger.info(&format!("Processing {} notifications", self.queue.len()));
        let mut sent = 0usize;
        let mut failed = 0usize;
        for notification in &mut self.queue {
            if notification.status == "pending" {
                notification.status = "sent".to_string();
                sent += 1;
            }
        }
        self.queue.retain(|n| n.status == "pending");
        (sent, failed)
    }
}

impl Service for NotificationManager {
    /// Initialize the notification manager.
    fn initialize(&mut self) -> Result<(), String> {
        self.inner.initialize()
    }

    /// Shut down the notification manager.
    fn shutdown(&mut self) -> Result<(), String> {
        self.queue.clear();
        self.inner.shutdown()
    }

    /// Return health status of the notification manager.
    fn health_check(&self) -> ServiceHealth {
        self.inner.health_check()
    }
}
