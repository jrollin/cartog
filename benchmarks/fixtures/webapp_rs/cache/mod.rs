pub mod redis;
pub mod memory;

/// Trait defining the interface for all cache implementations.
pub trait Cache {
    /// Retrieve a value by key from the cache.
    fn get(&self, key: &str) -> Option<String>;

    /// Store a key-value pair in the cache with optional TTL.
    fn set(&mut self, key: &str, value: &str, ttl_secs: Option<u64>);

    /// Delete a key from the cache, returning true if it existed.
    fn delete(&mut self, key: &str) -> bool;

    /// Clear all entries from the cache.
    fn clear(&mut self) -> usize;
}
