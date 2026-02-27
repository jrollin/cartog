use crate::utils::helpers::{get_logger, generate_request_id};
use crate::services::base::{Service, ServiceHealth, BaseServiceImpl};
use crate::services::payment::gateway::PaymentGateway;
use crate::app_errors::AppErrorExt;

/// Supported payment currencies.
const SUPPORTED_CURRENCIES: &[&str] = &["USD", "EUR", "GBP", "JPY", "CAD"];

/// The result of a payment processing operation.
pub struct PaymentResult {
    /// The transaction ID.
    pub transaction_id: String,
    /// The status of the payment.
    pub status: String,
    /// The amount charged.
    pub amount: f64,
    /// The currency used.
    pub currency: String,
}

/// Processes payments using a gateway and manages payment lifecycle.
pub struct PaymentProcessor {
    /// The underlying service state.
    inner: BaseServiceImpl,
    /// The payment gateway to use.
    gateway: PaymentGateway,
}

impl PaymentProcessor {
    /// Create a new payment processor with the given gateway.
    pub fn new(gateway: PaymentGateway) -> Self {
        let logger = get_logger("services.payment.processor");
        logger.info("Creating PaymentProcessor");
        Self {
            inner: BaseServiceImpl::new("payment_processor"),
            gateway,
        }
    }

    /// Process a payment for the given user.
    pub fn process_payment(
        &mut self,
        user_id: &str,
        amount: f64,
        currency: &str,
        source: &str,
    ) -> Result<PaymentResult, AppErrorExt> {
        let logger = get_logger("services.payment.processor");
        self.inner.require_initialized()
            .map_err(|e| AppErrorExt::Payment {
                transaction_id: None,
                message: e,
            })?;
        self.validate_payment(amount, currency)?;
        logger.info(&format!("Processing payment: user={}, amount={} {}", user_id, amount, currency));
        let txn_id = generate_request_id();
        let gateway_result = self.gateway.charge(amount, currency, source);
        if !gateway_result.success {
            return Err(AppErrorExt::Payment {
                transaction_id: Some(txn_id),
                message: gateway_result.message,
            });
        }
        logger.info(&format!("Payment completed: txn={}", txn_id));
        Ok(PaymentResult {
            transaction_id: txn_id,
            status: "completed".to_string(),
            amount,
            currency: currency.to_string(),
        })
    }

    /// Refund a previously completed payment.
    pub fn refund(&mut self, transaction_id: &str, reason: &str) -> Result<PaymentResult, AppErrorExt> {
        let logger = get_logger("services.payment.processor");
        logger.info(&format!("Refunding payment: txn={}, reason={}", transaction_id, reason));
        let gateway_result = self.gateway.refund_charge(transaction_id);
        if !gateway_result.success {
            return Err(AppErrorExt::Payment {
                transaction_id: Some(transaction_id.to_string()),
                message: gateway_result.message,
            });
        }
        Ok(PaymentResult {
            transaction_id: transaction_id.to_string(),
            status: "refunded".to_string(),
            amount: 0.0,
            currency: String::new(),
        })
    }

    /// Validate payment parameters before processing.
    fn validate_payment(&self, amount: f64, currency: &str) -> Result<(), AppErrorExt> {
        if !SUPPORTED_CURRENCIES.contains(&currency) {
            return Err(AppErrorExt::Validation {
                field: "currency".to_string(),
                message: format!("Unsupported currency: {}", currency),
            });
        }
        if amount <= 0.0 {
            return Err(AppErrorExt::Validation {
                field: "amount".to_string(),
                message: "Amount must be positive".to_string(),
            });
        }
        if amount > 999999.0 {
            return Err(AppErrorExt::Validation {
                field: "amount".to_string(),
                message: "Amount exceeds maximum".to_string(),
            });
        }
        Ok(())
    }
}

impl Service for PaymentProcessor {
    /// Initialize the payment processor.
    fn initialize(&mut self) -> Result<(), String> {
        self.inner.initialize()
    }

    /// Shut down the payment processor.
    fn shutdown(&mut self) -> Result<(), String> {
        self.inner.shutdown()
    }

    /// Return health status of the payment processor.
    fn health_check(&self) -> ServiceHealth {
        self.inner.health_check()
    }
}
