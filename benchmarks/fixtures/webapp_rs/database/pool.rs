use crate::utils::helpers::get_logger;

/// A handle representing a single borrowed connection from the pool.
pub struct ConnectionHandle {
    /// The connection identifier.
    pub id: String,
    /// Whether this handle is currently in use.
    pub in_use: bool,
    /// Number of queries executed on this handle.
    pub query_count: u64,
}

/// A pool of database connections with configurable size.
pub struct ConnectionPool {
    /// The data source name / connection string.
    dsn: String,
    /// Maximum number of connections in the pool.
    max_size: usize,
    /// All connection handles managed by this pool.
    connections: Vec<ConnectionHandle>,
    /// Whether the pool has been initialized.
    initialized: bool,
}

impl ConnectionPool {
    /// Create a new connection pool with the given DSN and size.
    pub fn new(dsn: &str, max_size: usize) -> Self {
        let logger = get_logger("database.pool");
        logger.info(&format!("Creating pool: dsn={}, size={}", dsn, max_size));
        Self {
            dsn: dsn.to_string(),
            max_size,
            connections: Vec::new(),
            initialized: false,
        }
    }

    /// Initialize the pool by creating all connections.
    pub fn initialize(&mut self) -> Result<(), String> {
        let logger = get_logger("database.pool");
        if self.initialized {
            return Ok(());
        }
        for i in 0..self.max_size {
            self.connections.push(ConnectionHandle {
                id: format!("conn-{}", i),
                in_use: false,
                query_count: 0,
            });
        }
        self.initialized = true;
        logger.info(&format!("Pool initialized with {} connections", self.max_size));
        Ok(())
    }

    /// Acquire a connection from the pool.
    pub fn get_connection(&mut self) -> Result<&mut ConnectionHandle, String> {
        let logger = get_logger("database.pool");
        if !self.initialized {
            self.initialize()?;
        }
        for conn in &mut self.connections {
            if !conn.in_use {
                conn.in_use = true;
                conn.query_count += 1;
                logger.info(&format!("Acquired connection {}", conn.id));
                return Ok(conn);
            }
        }
        Err("Connection pool exhausted".to_string())
    }

    /// Release a connection back to the pool by ID.
    pub fn release_connection(&mut self, conn_id: &str) {
        let logger = get_logger("database.pool");
        for conn in &mut self.connections {
            if conn.id == conn_id {
                conn.in_use = false;
                logger.info(&format!("Released connection {}", conn_id));
                return;
            }
        }
    }

    /// Return pool statistics.
    pub fn stats(&self) -> (usize, usize, usize) {
        let active = self.connections.iter().filter(|c| c.in_use).count();
        let idle = self.connections.len() - active;
        (self.connections.len(), active, idle)
    }

    /// Shut down the pool and drop all connections.
    pub fn shutdown(&mut self) {
        let logger = get_logger("database.pool");
        self.connections.clear();
        self.initialized = false;
        logger.info("Pool shut down");
    }
}
