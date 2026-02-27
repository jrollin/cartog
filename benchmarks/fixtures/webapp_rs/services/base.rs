use crate::utils::helpers::get_logger;

/// Core trait that all services must implement.
pub trait Service {
    /// Initialize the service and prepare resources.
    fn initialize(&mut self) -> Result<(), String>;

    /// Shut down the service and release resources.
    fn shutdown(&mut self) -> Result<(), String>;

    /// Check the health of this service.
    fn health_check(&self) -> ServiceHealth;
}

/// Health status returned by a service.
pub struct ServiceHealth {
    /// Name of the service.
    pub name: String,
    /// Whether the service is healthy.
    pub healthy: bool,
    /// Optional status message.
    pub message: Option<String>,
}

/// Base implementation of the Service trait.
pub struct BaseServiceImpl {
    /// The name of this service.
    pub name: String,
    /// Whether this service has been initialized.
    initialized: bool,
}

impl BaseServiceImpl {
    /// Create a new base service with the given name.
    pub fn new(name: &str) -> Self {
        let logger = get_logger("services.base");
        logger.info(&format!("Creating service: {}", name));
        Self {
            name: name.to_string(),
            initialized: false,
        }
    }

    /// Check whether the service is initialized and return an error if not.
    pub fn require_initialized(&self) -> Result<(), String> {
        if !self.initialized {
            return Err(format!("{} not initialized", self.name));
        }
        Ok(())
    }
}

impl Service for BaseServiceImpl {
    /// Initialize the base service.
    fn initialize(&mut self) -> Result<(), String> {
        let logger = get_logger("services.base");
        self.initialized = true;
        logger.info(&format!("{} initialized", self.name));
        Ok(())
    }

    /// Shut down the base service.
    fn shutdown(&mut self) -> Result<(), String> {
        let logger = get_logger("services.base");
        self.initialized = false;
        logger.info(&format!("{} shut down", self.name));
        Ok(())
    }

    /// Return health status of the base service.
    fn health_check(&self) -> ServiceHealth {
        ServiceHealth {
            name: self.name.clone(),
            healthy: self.initialized,
            message: if self.initialized {
                Some("OK".to_string())
            } else {
                Some("Not initialized".to_string())
            },
        }
    }
}
