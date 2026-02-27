use crate::utils::helpers::get_logger;
use crate::database::connection::{DatabaseConnection, QueryResult};

/// Query builder for user-related operations.
pub struct UserQueries<'a> {
    /// Reference to the database connection.
    db: &'a mut DatabaseConnection,
}

impl<'a> UserQueries<'a> {
    /// Create a new UserQueries instance.
    pub fn new(db: &'a mut DatabaseConnection) -> Self {
        Self { db }
    }

    /// Find a user by their email address.
    pub fn find_by_email(&mut self, email: &str) -> Result<Option<Vec<(String, String)>>, String> {
        let logger = get_logger("database.queries.user");
        logger.info(&format!("Finding user by email: {}", email));
        let result = self.db.execute_query("SELECT * FROM users WHERE email = ?", &[email])?;
        Ok(result.rows.into_iter().next())
    }

    /// Find all active users with an optional limit.
    pub fn find_active(&mut self, limit: usize) -> Result<Vec<Vec<(String, String)>>, String> {
        let logger = get_logger("database.queries.user");
        logger.info(&format!("Finding active users, limit={}", limit));
        let result = self.db.execute_query("SELECT * FROM users WHERE active = 1 LIMIT ?", &[&limit.to_string()])?;
        Ok(result.rows)
    }

    /// Search users by name or email pattern.
    pub fn search(&mut self, query: &str) -> Result<QueryResult, String> {
        let logger = get_logger("database.queries.user");
        logger.info(&format!("Searching users: {}", query));
        self.db.execute_query("SELECT * FROM users WHERE name LIKE ? OR email LIKE ?", &[query, query])
    }

    /// Soft-delete a user by setting their deleted_at timestamp.
    pub fn soft_delete(&mut self, user_id: &str) -> Result<bool, String> {
        let logger = get_logger("database.queries.user");
        logger.info(&format!("Soft-deleting user {}", user_id));
        let affected = self.db.update("users", user_id, &[("deleted_at", "now")])?;
        Ok(affected > 0)
    }
}

/// Query builder for session-related operations.
pub struct SessionQueries<'a> {
    /// Reference to the database connection.
    db: &'a mut DatabaseConnection,
}

impl<'a> SessionQueries<'a> {
    /// Create a new SessionQueries instance.
    pub fn new(db: &'a mut DatabaseConnection) -> Self {
        Self { db }
    }

    /// Find an active session by token hash.
    pub fn find_active_session(&mut self, token: &str) -> Result<Option<Vec<(String, String)>>, String> {
        let logger = get_logger("database.queries.session");
        logger.info("Finding active session by token");
        let result = self.db.execute_query("SELECT * FROM sessions WHERE token_hash = ?", &[token])?;
        Ok(result.rows.into_iter().next())
    }

    /// Create a new session record.
    pub fn create_session(&mut self, user_id: &str, token_hash: &str, ip: &str) -> Result<String, String> {
        let logger = get_logger("database.queries.session");
        logger.info(&format!("Creating session for user {}", user_id));
        self.db.insert("sessions", &[("user_id", user_id), ("token_hash", token_hash), ("ip_address", ip)])
    }

    /// Expire a session by its ID.
    pub fn expire_session(&mut self, session_id: &str) -> Result<bool, String> {
        let logger = get_logger("database.queries.session");
        logger.info(&format!("Expiring session {}", session_id));
        let affected = self.db.update("sessions", session_id, &[("expired_at", "now")])?;
        Ok(affected > 0)
    }
}

/// Query builder for payment-related operations.
pub struct PaymentQueries<'a> {
    /// Reference to the database connection.
    db: &'a mut DatabaseConnection,
}

impl<'a> PaymentQueries<'a> {
    /// Create a new PaymentQueries instance.
    pub fn new(db: &'a mut DatabaseConnection) -> Self {
        Self { db }
    }

    /// Find a payment by its transaction ID.
    pub fn find_by_transaction_id(&mut self, txn_id: &str) -> Result<Option<Vec<(String, String)>>, String> {
        let logger = get_logger("database.queries.payment");
        logger.info(&format!("Finding payment by txn: {}", txn_id));
        let result = self.db.execute_query("SELECT * FROM payments WHERE transaction_id = ?", &[txn_id])?;
        Ok(result.rows.into_iter().next())
    }

    /// Find all payments for a user, optionally filtered by status.
    pub fn find_user_payments(&mut self, user_id: &str, status: Option<&str>) -> Result<Vec<Vec<(String, String)>>, String> {
        let logger = get_logger("database.queries.payment");
        logger.info(&format!("Finding payments for user {}", user_id));
        match status {
            Some(s) => {
                let result = self.db.execute_query(
                    "SELECT * FROM payments WHERE user_id = ? AND status = ?",
                    &[user_id, s],
                )?;
                Ok(result.rows)
            }
            None => {
                let result = self.db.execute_query(
                    "SELECT * FROM payments WHERE user_id = ?",
                    &[user_id],
                )?;
                Ok(result.rows)
            }
        }
    }

    /// Create a new payment record.
    pub fn create_payment(&mut self, user_id: &str, amount: &str, currency: &str, txn_id: &str) -> Result<String, String> {
        let logger = get_logger("database.queries.payment");
        logger.info(&format!("Creating payment: {} {} for user {}", amount, currency, user_id));
        self.db.insert("payments", &[
            ("user_id", user_id),
            ("amount", amount),
            ("currency", currency),
            ("transaction_id", txn_id),
            ("status", "pending"),
        ])
    }

    /// Update the status of a payment.
    pub fn update_status(&mut self, txn_id: &str, status: &str) -> Result<bool, String> {
        let logger = get_logger("database.queries.payment");
        logger.info(&format!("Updating payment {} to {}", txn_id, status));
        let result = self.db.execute_query(
            "UPDATE payments SET status = ? WHERE transaction_id = ?",
            &[status, txn_id],
        )?;
        Ok(result.affected > 0)
    }
}
