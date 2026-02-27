use crate::utils::helpers::get_logger;
use crate::services::base::{Service, ServiceHealth, BaseServiceImpl};

/// An entry in the audit trail.
pub struct AuditEntry {
    /// The action that was performed.
    pub action: String,
    /// Who performed the action.
    pub actor: String,
    /// The resource affected.
    pub resource: String,
    /// Additional details about the action.
    pub details: String,
    /// Timestamp of the action.
    pub timestamp: u64,
}

/// Trait for services that support audit logging.
pub trait Auditable {
    /// Record an audit entry for the given action.
    fn record_audit(&mut self, action: &str, actor: &str, resource: &str, details: &str);

    /// Retrieve the audit trail, optionally filtered by resource.
    fn get_audit_trail(&self, resource: Option<&str>, limit: usize) -> Vec<&AuditEntry>;
}

/// A service that automatically records an audit trail.
pub struct AuditableService {
    /// The underlying service implementation.
    inner: BaseServiceImpl,
    /// The stored audit log entries.
    audit_log: Vec<AuditEntry>,
}

impl AuditableService {
    /// Create a new auditable service with the given name.
    pub fn new(name: &str) -> Self {
        let logger = get_logger("services.auditable");
        logger.info(&format!("Creating AuditableService: {}", name));
        Self {
            inner: BaseServiceImpl::new(name),
            audit_log: Vec::new(),
        }
    }
}

impl Auditable for AuditableService {
    /// Record an audit entry.
    fn record_audit(&mut self, action: &str, actor: &str, resource: &str, details: &str) {
        let logger = get_logger("services.auditable");
        logger.info(&format!("Audit: {} {} on {}", actor, action, resource));
        self.audit_log.push(AuditEntry {
            action: action.to_string(),
            actor: actor.to_string(),
            resource: resource.to_string(),
            details: details.to_string(),
            timestamp: 0,
        });
    }

    /// Get the audit trail, optionally filtered by resource.
    fn get_audit_trail(&self, resource: Option<&str>, limit: usize) -> Vec<&AuditEntry> {
        let logger = get_logger("services.auditable");
        logger.info(&format!("Getting audit trail, limit={}", limit));
        self.audit_log
            .iter()
            .filter(|e| resource.map_or(true, |r| e.resource == r))
            .rev()
            .take(limit)
            .collect()
    }
}

impl Service for AuditableService {
    /// Initialize the auditable service.
    fn initialize(&mut self) -> Result<(), String> {
        self.inner.initialize()
    }

    /// Shut down the auditable service.
    fn shutdown(&mut self) -> Result<(), String> {
        self.inner.shutdown()
    }

    /// Return health status of the auditable service.
    fn health_check(&self) -> ServiceHealth {
        self.inner.health_check()
    }
}
