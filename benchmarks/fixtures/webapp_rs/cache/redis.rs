use std::collections::HashMap;
use crate::utils::helpers::get_logger;
use crate::cache::Cache;

/// A simulated Redis-backed cache implementation.
pub struct RedisCache {
    /// The Redis connection URL.
    url: String,
    /// In-memory store simulating Redis.
    store: HashMap<String, String>,
    /// Whether the cache is connected.
    connected: bool,
}

impl RedisCache {
    /// Create a new Redis cache with the given connection URL.
    pub fn new(url: &str) -> Self {
        let logger = get_logger("cache.redis");
        logger.info(&format!("Creating RedisCache: url={}", url));
        Self {
            url: url.to_string(),
            store: HashMap::new(),
            connected: false,
        }
    }

    /// Connect to the Redis server.
    pub fn connect(&mut self) -> Result<(), String> {
        let logger = get_logger("cache.redis");
        self.connected = true;
        logger.info("Connected to Redis");
        Ok(())
    }

    /// Disconnect from the Redis server.
    pub fn disconnect(&mut self) {
        let logger = get_logger("cache.redis");
        self.connected = false;
        logger.info("Disconnected from Redis");
    }

    /// Check if connected to Redis.
    pub fn is_connected(&self) -> bool {
        self.connected
    }
}

impl Cache for RedisCache {
    /// Get a value from Redis by key.
    fn get(&self, key: &str) -> Option<String> {
        let logger = get_logger("cache.redis");
        let result = self.store.get(key).cloned();
        if result.is_some() {
            logger.info(&format!("Redis GET hit: {}", key));
        } else {
            logger.info(&format!("Redis GET miss: {}", key));
        }
        result
    }

    /// Set a value in Redis.
    fn set(&mut self, key: &str, value: &str, _ttl_secs: Option<u64>) {
        let logger = get_logger("cache.redis");
        self.store.insert(key.to_string(), value.to_string());
        logger.info(&format!("Redis SET: {}", key));
    }

    /// Delete a key from Redis.
    fn delete(&mut self, key: &str) -> bool {
        let logger = get_logger("cache.redis");
        let existed = self.store.remove(key).is_some();
        logger.info(&format!("Redis DEL: {} (existed={})", key, existed));
        existed
    }

    /// Clear all keys from Redis.
    fn clear(&mut self) -> usize {
        let logger = get_logger("cache.redis");
        let count = self.store.len();
        self.store.clear();
        logger.info(&format!("Redis FLUSHALL: {} keys", count));
        count
    }
}
