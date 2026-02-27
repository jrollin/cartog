use crate::utils::helpers::get_logger;
use crate::cache::Cache;
use crate::cache::memory::MemoryCache;

/// A background task for cleaning up expired data.
pub struct CleanupTask {
    /// The memory cache to clean.
    cache: MemoryCache,
    /// Number of cleanup runs completed.
    runs: u64,
}

impl CleanupTask {
    /// Create a new cleanup task.
    pub fn new() -> Self {
        let logger = get_logger("tasks.cleanup");
        logger.info("Creating CleanupTask");
        Self {
            cache: MemoryCache::new(1000),
            runs: 0,
        }
    }

    /// Run the cleanup task.
    pub fn run(&mut self) -> Result<usize, String> {
        let logger = get_logger("tasks.cleanup");
        logger.info("Running cleanup task");
        let cleared = self.cache.clear();
        self.runs += 1;
        logger.info(&format!("Cleanup complete: {} entries cleared, run #{}", cleared, self.runs));
        Ok(cleared)
    }

    /// Return the number of cleanup runs completed.
    pub fn stats(&self) -> u64 {
        self.runs
    }
}
