use std::collections::HashMap;

use crate::utils::helpers::get_logger;
use crate::services::base::{Service, ServiceHealth, BaseServiceImpl};

/// A service wrapper that adds in-memory caching capabilities.
pub struct CacheableService {
    /// The underlying service implementation.
    inner: BaseServiceImpl,
    /// The in-memory cache store.
    cache: HashMap<String, String>,
    /// Default time-to-live in seconds for cache entries.
    default_ttl: u64,
}

impl CacheableService {
    /// Create a new cacheable service with the given name.
    pub fn new(name: &str) -> Self {
        let logger = get_logger("services.cacheable");
        logger.info(&format!("Creating CacheableService: {}", name));
        Self {
            inner: BaseServiceImpl::new(name),
            cache: HashMap::new(),
            default_ttl: 300,
        }
    }

    /// Retrieve a value from the cache by key.
    pub fn cache_get(&self, key: &str) -> Option<&String> {
        let logger = get_logger("services.cacheable");
        match self.cache.get(key) {
            Some(val) => {
                logger.info(&format!("Cache hit: {}", key));
                Some(val)
            }
            None => {
                logger.info(&format!("Cache miss: {}", key));
                None
            }
        }
    }

    /// Store a value in the cache with the given key.
    pub fn cache_set(&mut self, key: &str, value: &str) {
        let logger = get_logger("services.cacheable");
        self.cache.insert(key.to_string(), value.to_string());
        logger.info(&format!("Cache set: {} (ttl={}s)", key, self.default_ttl));
    }

    /// Remove all entries from the cache, returning the number removed.
    pub fn cache_clear(&mut self) -> usize {
        let logger = get_logger("services.cacheable");
        let count = self.cache.len();
        self.cache.clear();
        logger.info(&format!("Cache cleared: {} entries", count));
        count
    }
}

impl Service for CacheableService {
    /// Initialize the cacheable service.
    fn initialize(&mut self) -> Result<(), String> {
        self.inner.initialize()
    }

    /// Shut down the cacheable service and clear cache.
    fn shutdown(&mut self) -> Result<(), String> {
        self.cache.clear();
        self.inner.shutdown()
    }

    /// Return health status of the cacheable service.
    fn health_check(&self) -> ServiceHealth {
        self.inner.health_check()
    }
}
