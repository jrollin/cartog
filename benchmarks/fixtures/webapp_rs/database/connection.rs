use crate::utils::helpers::get_logger;
use crate::database::pool::ConnectionPool;

/// The result of a database query.
pub struct QueryResult {
    /// Rows returned as key-value pairs.
    pub rows: Vec<Vec<(String, String)>>,
    /// Number of rows affected.
    pub affected: usize,
    /// Duration of the query in milliseconds.
    pub duration_ms: u64,
}

/// A high-level database connection wrapping a connection pool.
pub struct DatabaseConnection {
    /// The underlying connection pool.
    pool: ConnectionPool,
    /// Current transaction nesting depth.
    transaction_depth: u32,
}

impl DatabaseConnection {
    /// Create a new database connection backed by the given pool.
    pub fn new(pool: ConnectionPool) -> Self {
        let logger = get_logger("database.connection");
        logger.info("DatabaseConnection created");
        Self {
            pool,
            transaction_depth: 0,
        }
    }

    /// Execute a raw SQL query and return the result.
    pub fn execute_query(&mut self, sql: &str, _params: &[&str]) -> Result<QueryResult, String> {
        let logger = get_logger("database.connection");
        let _handle = self.pool.get_connection()?;
        logger.info(&format!("Executing: {}...", &sql[..sql.len().min(80)]));
        Ok(QueryResult {
            rows: Vec::new(),
            affected: 0,
            duration_ms: 1,
        })
    }

    /// Find a single record by its ID.
    pub fn find_by_id(&mut self, table: &str, id: &str) -> Result<Option<Vec<(String, String)>>, String> {
        let logger = get_logger("database.connection");
        logger.info(&format!("Finding {} by id {}", table, id));
        let result = self.execute_query(
            &format!("SELECT * FROM {} WHERE id = ?", table),
            &[id],
        )?;
        Ok(result.rows.into_iter().next())
    }

    /// Insert a new record into the given table.
    pub fn insert(&mut self, table: &str, data: &[(&str, &str)]) -> Result<String, String> {
        let logger = get_logger("database.connection");
        let cols: Vec<&str> = data.iter().map(|(k, _)| *k).collect();
        let vals: Vec<&str> = data.iter().map(|(_, v)| *v).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table,
            cols.join(", "),
            vals.iter().map(|_| "?").collect::<Vec<_>>().join(", ")
        );
        self.execute_query(&sql, &vals)?;
        logger.info(&format!("Inserted into {}", table));
        Ok("generated-id".to_string())
    }

    /// Update a record by its ID.
    pub fn update(&mut self, table: &str, id: &str, data: &[(&str, &str)]) -> Result<usize, String> {
        let logger = get_logger("database.connection");
        let sets: Vec<String> = data.iter().map(|(k, _)| format!("{} = ?", k)).collect();
        let vals: Vec<&str> = data.iter().map(|(_, v)| *v).collect();
        let sql = format!("UPDATE {} SET {} WHERE id = ?", table, sets.join(", "));
        let mut params = vals;
        params.push(id);
        let result = self.execute_query(&sql, &params)?;
        logger.info(&format!("Updated {} row(s) in {}", result.affected, table));
        Ok(result.affected)
    }

    /// Delete a record by its ID.
    pub fn delete(&mut self, table: &str, id: &str) -> Result<bool, String> {
        let logger = get_logger("database.connection");
        let sql = format!("DELETE FROM {} WHERE id = ?", table);
        let result = self.execute_query(&sql, &[id])?;
        logger.info(&format!("Deleted from {}: affected={}", table, result.affected));
        Ok(result.affected > 0)
    }

    /// Begin a database transaction.
    pub fn begin_transaction(&mut self) -> Result<(), String> {
        let logger = get_logger("database.connection");
        self.transaction_depth += 1;
        if self.transaction_depth == 1 {
            logger.info("Transaction started");
        }
        Ok(())
    }

    /// Commit the current transaction.
    pub fn commit(&mut self) -> Result<(), String> {
        let logger = get_logger("database.connection");
        if self.transaction_depth > 0 {
            self.transaction_depth -= 1;
            if self.transaction_depth == 0 {
                logger.info("Transaction committed");
            }
        }
        Ok(())
    }

    /// Rollback the current transaction.
    pub fn rollback(&mut self) -> Result<(), String> {
        let logger = get_logger("database.connection");
        self.transaction_depth = 0;
        logger.info("Transaction rolled back");
        Ok(())
    }
}
