use crate::utils::helpers::get_logger;

/// Possible states of a payment.
#[derive(Debug, Clone, PartialEq)]
pub enum PaymentStatus {
    /// Payment is waiting to be processed.
    Pending,
    /// Payment is currently being processed.
    Processing,
    /// Payment was successfully completed.
    Completed,
    /// Payment failed.
    Failed,
    /// Payment was refunded.
    Refunded,
}

/// A payment record in the system.
pub struct Payment {
    /// Unique payment identifier.
    pub id: u64,
    /// The ID of the user who made the payment.
    pub user_id: u64,
    /// The payment amount.
    pub amount: f64,
    /// The currency code.
    pub currency: String,
    /// External transaction identifier.
    pub transaction_id: String,
    /// Current payment status.
    pub status: PaymentStatus,
}

impl Payment {
    /// Create a new pending payment.
    pub fn new(id: u64, user_id: u64, amount: f64, currency: &str, txn_id: &str) -> Self {
        let logger = get_logger("models.payment");
        logger.info(&format!("Creating payment: {} {} {}", id, amount, currency));
        Self {
            id,
            user_id,
            amount,
            currency: currency.to_string(),
            transaction_id: txn_id.to_string(),
            status: PaymentStatus::Pending,
        }
    }

    /// Mark the payment as completed.
    pub fn complete(&mut self) {
        let logger = get_logger("models.payment");
        self.status = PaymentStatus::Completed;
        logger.info(&format!("Payment {} completed", self.transaction_id));
    }

    /// Mark the payment as failed.
    pub fn fail(&mut self, reason: &str) {
        let logger = get_logger("models.payment");
        self.status = PaymentStatus::Failed;
        logger.info(&format!("Payment {} failed: {}", self.transaction_id, reason));
    }

    /// Refund the payment.
    pub fn refund(&mut self) {
        let logger = get_logger("models.payment");
        self.status = PaymentStatus::Refunded;
        logger.info(&format!("Payment {} refunded", self.transaction_id));
    }

    /// Check if the payment is completed.
    pub fn is_completed(&self) -> bool {
        self.status == PaymentStatus::Completed
    }

    /// Find a payment by transaction ID (simulated).
    pub fn find_by_transaction_id(txn_id: &str) -> Option<Payment> {
        let logger = get_logger("models.payment");
        logger.info(&format!("Looking up payment by txn: {}", txn_id));
        None
    }
}
