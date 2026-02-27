use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// A simple logger that prefixes messages with a module name.
pub struct Logger {
    /// The name of the module this logger belongs to.
    pub name: String,
}

impl Logger {
    /// Log an informational message.
    pub fn info(&self, msg: &str) {
        println!("[{}] INFO: {}", self.name, msg);
    }

    /// Log a warning message.
    pub fn warn(&self, msg: &str) {
        println!("[{}] WARN: {}", self.name, msg);
    }

    /// Log an error message.
    pub fn error(&self, msg: &str) {
        eprintln!("[{}] ERROR: {}", self.name, msg);
    }
}

impl fmt::Display for Logger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Logger({})", self.name)
    }
}

/// Create a new logger instance for the given module name.
pub fn get_logger(name: &str) -> Logger {
    Logger {
        name: name.to_string(),
    }
}

/// Validate that a request has required fields (path, method).
pub fn validate_request(path: &str, method: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Request path cannot be empty".to_string());
    }
    if method.is_empty() {
        return Err("Request method cannot be empty".to_string());
    }
    Ok(())
}

/// Generate a unique request identifier based on timestamp.
pub fn generate_request_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("req-{}-{}", ts, ts % 1000)
}

/// Sanitize user input by removing control characters and trimming.
pub fn sanitize_input(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Paginate a slice of items, returning the requested page.
pub fn paginate<T: Clone>(items: &[T], page: usize, per_page: usize) -> Vec<T> {
    let start = (page.saturating_sub(1)) * per_page;
    items.iter().skip(start).take(per_page).cloned().collect()
}

/// Mask sensitive fields in a string value for safe logging.
pub fn mask_sensitive(value: &str) -> String {
    if value.len() > 4 {
        format!("{}***{}", &value[..2], &value[value.len() - 2..])
    } else {
        "***".to_string()
    }
}
