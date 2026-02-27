use crate::utils::helpers::get_logger;
use crate::database::connection::DatabaseConnection;

/// A single database migration with version, name, and SQL.
pub struct Migration {
    /// The migration version string.
    pub version: String,
    /// A human-readable name for the migration.
    pub name: String,
    /// The SQL to execute.
    pub sql: String,
}

/// Runs pending database migrations in order.
pub struct MigrationRunner {
    /// The database connection to run migrations against.
    db: DatabaseConnection,
    /// All known migrations.
    migrations: Vec<Migration>,
}

impl MigrationRunner {
    /// Create a new migration runner with the given connection.
    pub fn new(db: DatabaseConnection) -> Self {
        let logger = get_logger("database.migrations");
        logger.info("MigrationRunner initialized");
        let migrations = vec![
            Migration {
                version: "001".to_string(),
                name: "create_users".to_string(),
                sql: "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT)".to_string(),
            },
            Migration {
                version: "002".to_string(),
                name: "create_sessions".to_string(),
                sql: "CREATE TABLE sessions (id TEXT PRIMARY KEY, user_id TEXT)".to_string(),
            },
            Migration {
                version: "003".to_string(),
                name: "create_payments".to_string(),
                sql: "CREATE TABLE payments (id TEXT PRIMARY KEY, amount REAL)".to_string(),
            },
            Migration {
                version: "004".to_string(),
                name: "create_events".to_string(),
                sql: "CREATE TABLE events (id TEXT PRIMARY KEY, type TEXT)".to_string(),
            },
            Migration {
                version: "005".to_string(),
                name: "create_notifications".to_string(),
                sql: "CREATE TABLE notifications (id TEXT PRIMARY KEY, user_id TEXT)".to_string(),
            },
        ];
        Self { db, migrations }
    }

    /// Run all pending migrations, returning the count applied.
    pub fn run_pending(&mut self) -> Result<u32, String> {
        let logger = get_logger("database.migrations");
        let mut count = 0u32;
        for migration in &self.migrations {
            logger.info(&format!("Applying migration {}: {}", migration.version, migration.name));
            self.db.begin_transaction()?;
            match self.db.execute_query(&migration.sql, &[]) {
                Ok(_) => {
                    self.db.commit()?;
                    count += 1;
                }
                Err(e) => {
                    self.db.rollback()?;
                    return Err(format!("Migration {} failed: {}", migration.version, e));
                }
            }
        }
        logger.info(&format!("{} migrations applied", count));
        Ok(count)
    }

    /// Return migration status information.
    pub fn status(&self) -> (usize, usize) {
        let logger = get_logger("database.migrations");
        let total = self.migrations.len();
        logger.info(&format!("{} total migrations", total));
        (0, total)
    }
}
