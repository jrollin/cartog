use std::collections::HashMap;
use crate::utils::helpers::get_logger;
use crate::cache::Cache;

/// An in-memory cache implementation using a HashMap.
pub struct MemoryCache {
    /// The in-memory key-value store.
    store: HashMap<String, String>,
    /// Maximum number of entries before eviction.
    max_size: usize,
}

impl MemoryCache {
    /// Create a new in-memory cache with the given max size.
    pub fn new(max_size: usize) -> Self {
        let logger = get_logger("cache.memory");
        logger.info(&format!("Creating MemoryCache: max_size={}", max_size));
        Self {
            store: HashMap::new(),
            max_size,
        }
    }

    /// Return the current number of entries in the cache.
    pub fn size(&self) -> usize {
        self.store.len()
    }

    /// Check if the cache is at capacity.
    pub fn is_full(&self) -> bool {
        self.store.len() >= self.max_size
    }
}

impl Cache for MemoryCache {
    /// Get a value from the memory cache.
    fn get(&self, key: &str) -> Option<String> {
        let logger = get_logger("cache.memory");
        let result = self.store.get(key).cloned();
        if result.is_some() {
            logger.info(&format!("Memory GET hit: {}", key));
        } else {
            logger.info(&format!("Memory GET miss: {}", key));
        }
        result
    }

    /// Set a value in the memory cache.
    fn set(&mut self, key: &str, value: &str, _ttl_secs: Option<u64>) {
        let logger = get_logger("cache.memory");
        if self.store.len() >= self.max_size && !self.store.contains_key(key) {
            logger.warn("Memory cache at capacity, evicting oldest");
            if let Some(first_key) = self.store.keys().next().cloned() {
                self.store.remove(&first_key);
            }
        }
        self.store.insert(key.to_string(), value.to_string());
        logger.info(&format!("Memory SET: {}", key));
    }

    /// Delete a key from the memory cache.
    fn delete(&mut self, key: &str) -> bool {
        let logger = get_logger("cache.memory");
        let existed = self.store.remove(key).is_some();
        logger.info(&format!("Memory DEL: {} (existed={})", key, existed));
        existed
    }

    /// Clear all entries from the memory cache.
    fn clear(&mut self) -> usize {
        let logger = get_logger("cache.memory");
        let count = self.store.len();
        self.store.clear();
        logger.info(&format!("Memory CLEAR: {} entries", count));
        count
    }
}
