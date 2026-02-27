use crate::utils::helpers::{get_logger, sanitize_input, generate_request_id};
use crate::services::base::{Service, ServiceHealth, BaseServiceImpl};

/// An email message to be sent.
pub struct EmailMessage {
    /// The recipient email address.
    pub to: String,
    /// The email subject.
    pub subject: String,
    /// The email body content.
    pub body: String,
    /// A unique message ID.
    pub message_id: String,
}

/// Sends emails using a configured SMTP-like backend.
pub struct EmailSender {
    /// The underlying service state.
    inner: BaseServiceImpl,
    /// The sender email address.
    from_address: String,
    /// Number of emails sent in this session.
    sent_count: u64,
}

impl EmailSender {
    /// Create a new email sender with the given from address.
    pub fn new(from_address: &str) -> Self {
        let logger = get_logger("services.email");
        logger.info(&format!("Creating EmailSender from={}", from_address));
        Self {
            inner: BaseServiceImpl::new("email_sender"),
            from_address: from_address.to_string(),
            sent_count: 0,
        }
    }

    /// Send a plain-text email to the given recipient.
    pub fn send(&mut self, to: &str, subject: &str, body: &str) -> Result<EmailMessage, String> {
        let logger = get_logger("services.email");
        self.inner.require_initialized()?;
        let clean_subject = sanitize_input(subject);
        let clean_body = sanitize_input(body);
        let message_id = generate_request_id();
        logger.info(&format!("Sending email to={}, subject={}", to, clean_subject));
        self.sent_count += 1;
        Ok(EmailMessage {
            to: to.to_string(),
            subject: clean_subject,
            body: clean_body,
            message_id,
        })
    }

    /// Send an email using a named template with variables.
    pub fn send_template(
        &mut self,
        to: &str,
        template: &str,
        vars: &[(&str, &str)],
    ) -> Result<EmailMessage, String> {
        let logger = get_logger("services.email");
        self.inner.require_initialized()?;
        let mut body = template.to_string();
        for (key, value) in vars {
            body = body.replace(&format!("{{{{{}}}}}", key), value);
        }
        logger.info(&format!("Sending template email to={}", to));
        self.send(to, template, &body)
    }

    /// Return the total number of emails sent.
    pub fn stats(&self) -> u64 {
        self.sent_count
    }
}

impl Service for EmailSender {
    /// Initialize the email sender.
    fn initialize(&mut self) -> Result<(), String> {
        self.inner.initialize()
    }

    /// Shut down the email sender.
    fn shutdown(&mut self) -> Result<(), String> {
        self.inner.shutdown()
    }

    /// Return health status of the email sender.
    fn health_check(&self) -> ServiceHealth {
        self.inner.health_check()
    }
}
