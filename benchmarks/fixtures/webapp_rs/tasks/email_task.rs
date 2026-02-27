use crate::utils::helpers::get_logger;
use crate::services::email::sender::EmailSender;
use crate::services::base::Service;

/// A background task for sending pending emails.
pub struct EmailTask {
    /// The email sender instance.
    sender: EmailSender,
    /// Number of emails processed.
    processed: u64,
}

impl EmailTask {
    /// Create a new email task.
    pub fn new(from_address: &str) -> Self {
        let logger = get_logger("tasks.email");
        logger.info("Creating EmailTask");
        Self {
            sender: EmailSender::new(from_address),
            processed: 0,
        }
    }

    /// Run the email task, processing all pending emails.
    pub fn run(&mut self) -> Result<u64, String> {
        let logger = get_logger("tasks.email");
        self.sender.initialize()?;
        logger.info("Running email task");
        self.sender.send("user@example.com", "Welcome", "Hello!")?;
        self.processed += 1;
        logger.info(&format!("Email task complete: {} processed", self.processed));
        Ok(self.processed)
    }

    /// Return the number of emails processed.
    pub fn stats(&self) -> u64 {
        self.processed
    }
}
