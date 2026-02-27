use crate::utils::helpers::get_logger;
use crate::services::payment::processor::PaymentProcessor;
use crate::services::payment::gateway::PaymentGateway;
use crate::services::base::Service;

/// A background task for processing pending payments.
pub struct PaymentTask {
    /// The payment processor instance.
    processor: PaymentProcessor,
    /// Number of payments processed.
    processed: u64,
}

impl PaymentTask {
    /// Create a new payment task with the given API key.
    pub fn new(api_key: &str) -> Self {
        let logger = get_logger("tasks.payment");
        logger.info("Creating PaymentTask");
        let gateway = PaymentGateway::new(api_key, "sandbox");
        Self {
            processor: PaymentProcessor::new(gateway),
            processed: 0,
        }
    }

    /// Run the payment task, processing pending payments.
    pub fn run(&mut self) -> Result<u64, String> {
        let logger = get_logger("tasks.payment");
        self.processor.initialize()?;
        logger.info("Running payment task");
        match self.processor.process_payment("user-1", 99.99, "USD", "card_test") {
            Ok(result) => {
                self.processed += 1;
                logger.info(&format!("Payment processed: txn={}", result.transaction_id));
            }
            Err(e) => {
                logger.error(&format!("Payment failed: {}", e));
            }
        }
        Ok(self.processed)
    }

    /// Return the number of payments processed.
    pub fn stats(&self) -> u64 {
        self.processed
    }
}
